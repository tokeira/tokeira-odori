# Spike: driving headless Claude Code from Rust

Proves the supervision model the O3 Claude provider stands on: spawn
`claude -p --output-format stream-json --verbose`, parse the event stream
line-by-line, capture the terminal result, resume a session by id, and map
exit codes into a retry taxonomy. Probed against **claude 2.1.220**; every
finding below is version-stamped by that.

```bash
cargo run -- turn                 # one fresh turn; prints the session id
cargo run -- resume <id>          # follow-up turn against that session
cargo run -- taxonomy             # the failure shapes a provider must classify
```

`mcp_probe_server.py` is the minimal MCP stdio server used by the
crash-mid-tool-call experiment (below): `MODE=block` never answers a
`tools/call` so the harness can be killed mid-await; `MODE=respond` answers
immediately; every inbound message is logged verbatim for identity
comparison.

## Findings

### Protocol shape (observed)

- stdout is JSONL, one event per line; `--verbose` is **mandatory** with
  `-p --output-format stream-json` (without it the CLI rejects the combo).
- Happy-path sequence for a tool-using turn: `system:init` →
  `assistant` (may open with `thinking` blocks; `tool_use` blocks carry the
  call ids) → `user` (tool results echoed, `tool_use_id` matching) → …
  repeat … → `assistant` (final text) → `system:post_turn_summary` →
  `result`.
- Also observed live: top-level **`rate_limit_event`** (no `system`
  wrapper), `system:thinking_tokens`, `system:task_summary`, and a
  `server/discover` MCP-side call before `initialize`. Unknown-event
  tolerance is not optional — new kinds appear mid-stream on ordinary runs.
- **Trap:** `result` is semantically terminal but **not always the last
  line** — a `system:task_summary` was observed *after* `result`. Treat
  `result` as the answer, then drain to EOF; never stop reading at `result`.
- `system:init` arrives ~1 s from spawn on warm/failure runs and took up to
  ~4.9 s on a fresh live turn. The provider should treat it as **liveness
  heartbeat №1**.
- `result` carries `is_error`, `subtype`, `num_turns`, `total_cost_usd`,
  `usage`, `duration_ms`/`duration_api_ms`, and `terminal_reason` —
  **`"completed"` on success**, `"api_error"` on API failure. So the field
  is always populated; key success on `is_error`, never on
  `subtype`/`terminal_reason` shape alone (an auth-failure run observed
  `subtype: "success"` with `is_error: true`).
- **Trap:** on `--resume` of a nonexistent session, `result.session_id`
  echoes the *requested* id (`subtype: "error_during_execution"`,
  `num_turns: 0`) — not proof the session exists. The human-readable cause
  lands on **stderr**, not the event stream. Capture stderr alongside
  stdout.
- On an API-level failure the CLI emits a synthetic assistant message
  (`model: "<synthetic>"`) before the error result — filter these out of
  history by model name.

### Session resume (verified live)

`--resume <id>` retains history across processes: a session that answered
"pirouette" answered a follow-up "what word did you reply with before?" with
"pirouette", same session id, `is_error: false`. Resume startup is faster
than fresh (~1.0 s to init vs ~4.9 s).

### Exit codes

| shape | exit | stream |
| --- | --- | --- |
| clean turn (verified live) | 0 | full sequence, `result.is_error: false`, `terminal_reason: "completed"` |
| API/auth failure | 1 | `init` → synthetic `assistant` → `result` (`terminal_reason: "api_error"`) |
| resume of missing session | 1 | `result` only (`error_during_execution`), reason on stderr |
| unknown flag | 1 | **no stdout at all**; usage error on stderr, ~160 ms |
| killed by signal (SIGTERM) | 143 / signal | stream truncated, **no `result` event** |

Exit codes alone separate nothing (everything non-zero is 1, signals aside):
the taxonomy must be **(exit code, did-a-result-event-arrive,
terminal_reason, stderr)**. Provider mapping: no `result` event →
bridge/spawn defect or harness death (retryable); `result` with `api_error`
→ retryable with backoff (the CLI already retries internally — `api_retry`
events, observed `max_retries: 10`); missing-session on resume →
non-retryable, surface to the run loop.

### The crash-mid-tool-call experiment (spec Q2)

Setup: an MCP stdio server (`mcp_probe_server.py`, `MODE=block`) that never
answers `tools/call`; a turn is driven into calling its tool, the CLI is
killed (SIGTERM) mid-await, and the session is resumed with the server in
`MODE=respond`.

1. **The harness call id crosses the MCP boundary.** The `tools/call`
   request carries `params._meta["claudecode/toolUseId"]` —
   byte-identical to the `tool_use` block id in the stream
   (`toolu_01WgWpsQQuz8dY8PoBpsBGo6` in both). The JSON-RPC `id` is a
   per-connection counter (not an identity). A `progressToken` is present,
   so progress-notification keepalive has a target. Note the vendor
   namespace: Codex will need its own probe.
2. **Call ids are NOT stable across kill/resume.** On kill, Claude Code
   closed the pending call *in session history* as a failed tool result
   ("Connection closed"). The resumed model saw that failure, **regenerated
   the call under a fresh id** (`toolu_01UP7g92hR2N8tzU3GM3kgoj`), got the
   response, and completed normally — even narrating "the first attempt
   failed with `Connection closed`".

Consequence for the mcp-bridge spec: cross-attempt dedupe keyed on call id
will essentially never hit for harness-death recovery on this harness — the
regenerated-id path is the *main* path, not a residual. Same-id dedupe still
covers same-attempt MCP client retries and any resume that re-issues without
regeneration; but recovery-without-re-execution needs the workflow side
(orphaned in-flight policy, spec Q6) and/or heuristic dedupe (spec Q3), or
acceptance that harness-death recovery re-executes framework tools (their
injected idempotency key doing the real work). Not yet probed: whether the
CLI's MCP client retries a *timed-out* call within one attempt, and with
which id.

### Environment hygiene (the surprise finding)

A harness spawned from inside another agent session inherits that session's
`ANTHROPIC_BASE_URL` (host-side proxy) and fails hard with 401 against it.
The provider must **scrub inherited vendor transport/credential variables**
(`ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_API_KEY`) so the
harness authenticates from its own credential store. This also means
nested-agent topologies (an Odori app run *by* an agent) work only with
deliberate env policy — worth a line in the provider docs.

### Flag surface relevant to O3 (claude 2.1.220)

- `--resume <id>` / `--fork-session` (resume into a **new** session id —
  likely the right default for turn *retries*, keeping each attempt's
  history divergence isolated), `-c/--continue`.
- `--input-format stream-json` — bidirectional streaming exists; one-shot
  `-p` per turn is still the simpler v0 model.
- `--include-partial-messages` — token-level chunks; useful later for
  finer-grained heartbeats.
- `--max-budget-usd`, `--json-schema` (typed outputs!), `--mcp-config`
  (the O6 bridge's injection point — verified live with a stdio server),
  `--permission-mode`, `--allowedTools`/`--disallowedTools` (verified live:
  `mcp__<server>__<tool>` naming), `--no-session-persistence`, `--model`,
  `--fallback-model`, `--effort`.

### Timings (Apple Silicon laptop, subscription auth)

- Fresh live turn: first event ~4.9 s, trivial one-word turn ~8.6 s total.
- Resumed turn: first event ~1.0 s, ~4.7 s total.
- Failure paths: first event 0.5–1.1 s; flag error exits in ~0.16 s.
- A provider timeout budget must cover CLI startup + model latency;
  per-turn ceilings belong in activity start-to-close timeouts with
  event-driven heartbeats, not in the driver.

## What this settles for O3

Supervised one-shot subprocess per turn is viable and simple: spawn with
scrubbed env + piped stdio + kill-on-drop, stream-parse stdout with
unknown-event tolerance, drain past `result` to EOF, capture stderr, join on
exit, classify by the 4-tuple above. Session resume by id is real and fast;
`--fork-session` is the retry-isolation primitive. For O6: the bridge gets
the true harness call id via `_meta["claudecode/toolUseId"]`, keepalive has
a `progressToken` to target, and harness-death recovery must assume
regenerated call ids (see the experiment above and spec Q2/Q3/Q6).
