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

## Status: partially blocked on local CLI auth

The machine's standalone `claude` OAuth session is expired and cannot be
refreshed non-interactively (this agent session's own auth rides the host
app's proxy, which a subprocess cannot borrow). Until `claude login` /
`claude setup-token` is re-run interactively, live-turn findings (happy-path
event sequence with tool use, resume retention, real timings) are pending —
everything below is ground truth from the failure paths, which the CLI
exercises fully. The driver is finished and will produce the live findings
unchanged once auth is restored.

## Findings

### Protocol shape (observed)

- stdout is JSONL, one event per line; `--verbose` is **mandatory** with
  `-p --output-format stream-json` (without it the CLI rejects the combo).
- Event kinds observed: `system` (subtypes `init`, `api_retry`),
  `assistant`, and exactly one terminal `result` — always last, present
  even on failed runs. `user` events carry tool results in agentic turns
  (not yet observed live).
- `system:init` arrives first (~0.5–1.1 s from spawn) and carries
  `session_id`, `cwd`, `model`, and the tool list. The provider should
  treat it as its **liveness heartbeat №1**.
- `result` carries `is_error`, `subtype`, `num_turns`, `total_cost_usd`,
  `usage`, `duration_ms`/`duration_api_ms`, and (on failure) a
  `terminal_reason` (`api_error` observed).
- **Trap:** `result.subtype` can be `"success"` while `is_error: true`
  (auth-failure run). Key on `is_error`, never on `subtype`.
- **Trap:** on `--resume` of a nonexistent session, `result.session_id`
  echoes the *requested* id (`subtype: "error_during_execution"`,
  `num_turns: 0`) — it is not proof the session exists. The human-readable
  cause ("No conversation found with session ID: …") goes to **stderr**,
  not the event stream. Supervisors must capture stderr alongside stdout.
- On an API-level failure the CLI emits a synthetic assistant message
  (`model: "<synthetic>"`) before the error result — filter these out of
  history by model name.

### Exit codes

| shape | exit | stream |
| --- | --- | --- |
| clean turn | 0 (expected; unverified pending auth) | full sequence, `result.is_error: false` |
| API/auth failure | 1 | `init` → synthetic `assistant` → `result` (`terminal_reason: "api_error"`) |
| resume of missing session | 1 | `result` only (`error_during_execution`), reason on stderr |
| unknown flag | 1 | **no stdout at all**; usage error on stderr, ~160 ms |

Exit codes alone separate nothing (everything non-zero is 1): the taxonomy
must be **(exit code, did-a-result-event-arrive, terminal_reason, stderr)**.
Provider mapping: no `result` event → bridge/spawn defect (non-retryable
config error or retryable spawn flake); `result` with `api_error` →
retryable with backoff (the CLI already retries internally — `api_retry`
events, observed `max_retries: 10`); missing-session on resume →
non-retryable, surface to the run loop (history is gone; the turn must be
replayed from workflow history, not resumed).

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
  (the O6 bridge's injection point), `--permission-mode`,
  `--allowedTools`/`--disallowedTools`, `--no-session-persistence`,
  `--model`, `--fallback-model`, `--effort`.

### Timings (failure paths only, Apple Silicon laptop)

- Spawn → first event: 0.5–1.1 s. Spawn → exit on flag error: ~0.16 s.
- A provider timeout budget must cover CLI startup (~1 s) + model latency;
  per-turn ceilings belong in activity start-to-close timeouts with
  event-driven heartbeats, not in the driver.

## What this settles for O3

Supervised one-shot subprocess per turn is viable and simple: spawn with
scrubbed env + piped stdio + kill-on-drop, stream-parse stdout with
unknown-event tolerance, capture stderr, join on exit, classify by the
4-tuple above. Session resume by id is the recovery primitive;
`--fork-session` is the retry-isolation primitive. Open items pending auth:
happy-path sequence incl. `user`/tool events, resume retention proof, and
real turn timings.
