# Spike: driving Codex app-server from Rust

Phase-1 evidence for the O4 Codex subscription provider. The probe supervises
`codex app-server --listen stdio:// --strict-config`, speaks its
newline-delimited JSON-RPC protocol, starts/resumes/forks persisted threads,
streams notifications through turn completion, and attaches the included
streamable-HTTP MCP probe server.

Tested live on **2026-08-21** with **`codex-cli 0.148.0-alpha.15`** from the
ChatGPT macOS app bundle.
All behavioral claims below refer to that exact pin. Protocol field shape was
also checked against that binary's `app-server generate-json-schema
--experimental` and `generate-ts --experimental` output. The official
[app-server documentation](https://learn.chatgpt.com/docs/app-server),
[MCP guide](https://learn.chatgpt.com/docs/extend/mcp), and
[configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
are the external ground truth for documented defaults.

## Running the probes

```bash
cargo run -- turn
cargo run -- resume <thread-id>
cargo run -- fork <thread-id>

TOKEN=odori-probe-token python3 mcp_probe_server.py
cargo run -- mcp http://127.0.0.1:8765/mcp odori-probe-token

# Crash with tools/call blocked, restart the server in respond mode, resume.
PORT=8766 TOKEN=odori-probe-token MODE=block python3 mcp_probe_server.py
cargo run -- mcp-crash http://127.0.0.1:8766/mcp odori-probe-token
PORT=8766 TOKEN=odori-probe-token MODE=respond python3 mcp_probe_server.py
cargo run -- mcp-resume <thread-id> http://127.0.0.1:8766/mcp odori-probe-token

cargo run -- missing
cargo run -- bad-config
```

## Findings

### Q1: MCP attachment — HTTP works; no re-exec shim is needed

Codex 0.148 accepts both streamable HTTP and stdio MCP servers. The app-server
session can be given a loopback HTTP endpoint and authorization at
`thread/start`, `thread/resume`, or `thread/fork`. The `config` field is a raw
override map: **each key must use the same dotted path syntax as a CLI `-c`
override**. A nested JSON `mcp_servers` object is not equivalent; the first
probe using that shape registered the server but timed out before issuing an
HTTP request. This is the working shape:

```json
{
  "method": "thread/start",
  "id": 2,
  "params": {
    "config": {
      "mcp_servers.odori.url": "http://127.0.0.1:8765/mcp",
      "mcp_servers.odori.env_http_headers": {
        "Authorization": "ODORI_CODEX_MCP_HEADER_0_0"
      },
      "mcp_servers.odori.required": true,
      "mcp_servers.odori.enabled_tools": ["deploy"],
      "mcp_servers.odori.startup_timeout_sec": 10,
      "mcp_servers.odori.tool_timeout_sec": 120
    }
  }
}
```

Set `ODORI_CODEX_MCP_HEADER_0_0` to the complete `Bearer <token>` header value
on the app-server process. `env_http_headers` maps each header name to its
environment-variable name, so the per-attempt bridge token is not serialized
into persisted thread configuration.

The captured request carried
`authorization: Bearer odori-probe-token` on `initialize`,
`notifications/initialized`, `tools/list`, and `tools/call`. The bridge contract
is direct streamable HTTP; no stdio re-exec shim is required.

### Tool-call identity — `_meta.callId`, and it regenerates after death

The live `tools/call` request was:

```json
{
  "id": 2,
  "method": "tools/call",
  "params": {
    "_meta": {
      "callId": "exec-bf4952e5-b0d2-4260-881d-d8a656e1dbfc",
      "threadId": "01a024a1-2056-7d22-8208-465c3444b48f",
      "progressToken": 1,
      "x-codex-turn-metadata": {
        "session_id": "01a024a1-2056-7d22-8208-465c3444b48f",
        "thread_id": "01a024a1-2056-7d22-8208-465c3444b48f",
        "turn_id": "01a024a1-20ee-7863-ad59-cdf204db017b"
      }
    },
    "name": "odori_probe",
    "arguments": {}
  }
}
```

`params._meta.callId` is the harness identity the bridge must key on. It was
byte-identical to app-server's `item/started.params.item.id` for the
`mcpToolCall`. JSON-RPC `id` was only the MCP connection counter (`2`) and is
not call identity. `progressToken` is present for the bridge's progress
notifications. `x-codex-turn-metadata` also carries session/thread/turn
coordinates; the provider adds Odori run/turn/attempt strings to the same
metadata through `responsesapiClientMetadata`, but the per-attempt bearer token
remains the bridge's fencing authority.

Call ids are **not stable across app-server death and resume**. The blocked
pre-crash call used `exec-50829cb2-872d-48a5-ae75-ef92dee883cc`. After killing
app-server with that HTTP call in flight, restarting it, resuming the same
thread, and asking Codex to retry, the call used
`exec-9c1d7f3c-578b-4840-9e97-6a91c253527e`. This matches the Claude finding:
same-id registry dedupe still handles client replay, but crash recovery normally
takes the regenerated-id path and depends on framework-tool idempotency.

### Timeout knobs — per-server and spawn-pinnable

Codex documents and accepts:

- `mcp_servers.<id>.startup_timeout_sec` (default 10 seconds), with
  `startup_timeout_ms` as a millisecond alias.
- `mcp_servers.<id>.tool_timeout_sec` (default 60 seconds per call).

Both are accepted in the app-server session's dotted `config` overrides, so
they are fixed before MCP initialization. A live blocking-tool probe pinned
`tool_timeout_sec` to `5`; app-server completed the `mcpToolCall` as `failed`
with `durationMs: 5001` and
`timed out awaiting tools/call after 5s`. There is no separate app-server
whole-turn timeout in this protocol surface; Odori applies `TurnRequest`'s
provider deadline around the supervised process.

The MCP request carries `progressToken`, so the bridge can target
`notifications/progress`, but a live bridge probe found that Codex 0.148's
timeout is a **fixed wall-clock ceiling**. With `tool_timeout_sec = 1`, a durable
update lasting 1.5 seconds, and bridge progress every 250 ms, Codex still failed
at exactly 1000 ms. Progress does not reset this client's timer. The provider
does render the bridge timeout pin to every attached server, and
`BridgeConfig::validate` still enforces `keepalive < tool_timeout_sec`, but for
Codex the pin must also exceed the longest expected end-to-end tool execution.
This contradicts the bridge spec's general "progress resets a compliant MCP
client timeout" assumption and needs an operator decision on the production
pin (or a separate asynchronous-tool design); the frozen provider trait itself
already carries the required timeout value.

### Session lifecycle and event stream

One process can host many threads, but one app-server process per Odori harness
turn is viable and isolates activity cancellation. The sequence is:

1. Spawn app-server with piped stdin/stdout/stderr and kill-on-drop.
2. Send `initialize` once, await its response, then send `initialized`.
3. Send `thread/start`, `thread/resume`, or `thread/fork`.
4. Record `result.thread.id`, then send `turn/start` with text input.
5. Treat every notification as liveness; collect terminal text from completed
   `agentMessage` items whose phase is `final_answer` (or unknown, for legacy
   compatibility).
6. Treat matching `turn/completed` as semantic terminal, close stdin, and
   drain the process. Graceful terminal runs exited 0.

Observed notifications on ordinary turns included `thread/started`,
`thread/status/changed`, `turn/started`, `item/started`, `item/completed`,
`item/agentMessage/delta`, `thread/tokenUsage/updated`,
`account/rateLimits/updated`, MCP startup-status updates, and
`turn/completed`. Unknown-notification tolerance is required. Notifications can
arrive before the JSON-RPC response to `thread/start`, so a client must
demultiplex by response id rather than assuming response-first ordering.

Resume is durable across process death. A fresh thread that replied
`pirouette` was resumed by id in a new app-server process and correctly recalled
`pirouette`; `thread.id` stayed the same. `thread/fork` retained the history and
returned a new id. Live timings on the test machine were about 0.38 seconds to
the first fresh-turn notification and 3.26 seconds total; resume was about
0.13 seconds to first notification and 3.01 seconds total.

### Failure and exit taxonomy

App-server failures are richer than process exit codes, so the provider keys on
process status, whether `turn/completed` arrived, its `turn.status`, the
structured `turn.error`, JSON-RPC errors, and stderr:

| Observed/schema-grounded shape | Process exit | Provider class |
| --- | --- | --- |
| completed turn, then stdin EOF | 0 | success |
| stdout EOF/process death before `turn/completed` | signal/any | `HarnessDied` (retryable; resume heartbeat session) |
| `thread/resume` missing rollout: JSON-RPC `-32600`, `no rollout found for thread id ...` | provider terminates process | `SessionNotFound` (non-retryable) |
| required MCP initialization failure | provider terminates process | `Tooling` (retryable) |
| strict unknown config: JSON-RPC `-32600`, `unknown configuration field ...` | provider terminates process | `Config` (non-retryable) |
| unauthenticated live turn | 0 after terminal failure | `Config` with `codex login` remediation |
| `codexErrorInfo = usageLimitExceeded`, `sessionBudgetExceeded`, or overload/429 message | terminal failure | `Api` (retryable with activity backoff) |

The unauthenticated probe used an isolated empty `CODEX_HOME`. Codex emitted ten
top-level `error` notifications while its own retries ran, then
`turn/completed` with `status: failed`. A trap: this pin labeled the terminal
`codexErrorInfo` as `other`; the actionable message contained
`401 Unauthorized: Missing bearer ...`. Classification therefore also inspects
the message, and tells the operator to run `codex login` and
`codex login status`.

The generated schema exposes `error.params.willRetry`; retrying notifications
are liveness, not terminal results. It also exposes terminal
`codexErrorInfo` variants including `usageLimitExceeded`,
`sessionBudgetExceeded`, `serverOverloaded`, `unauthorized`, HTTP/response-stream
failures, and `responseTooManyFailedAttempts`. Usage-cap exhaustion was not
deliberately induced against the subscription, so its mapping is
schema-grounded and covered by the scripted provider regression rather than a
live quota-destructive probe. Rate/usage-cap surfaces map to retryable `Api`,
never a hang or terminal configuration failure.

### Documented fallback: `codex exec --json`

The fallback was exercised against the same HTTP MCP server. It emitted JSONL
`thread.started` → `turn.started` → `item.started`/`item.completed` (including
the MCP call) → `turn.completed`, then exited 0. It uses the same MCP config
keys and emitted the same `_meta.callId`. App-server remains the selected O4
transport because its session operations, structured terminal errors, and
notification stream map directly onto the frozen provider trait.

## Provider consequences

The O4 provider can implement the frozen trait without changing it: one
supervised app-server process per turn; start/resume/fork by recorded thread id;
all notifications as heartbeats; structured terminal usage; retryable API,
harness, timeout, and tooling failures; and direct HTTP bridge attachment with
the timeout pin. It warns when the installed CLI differs from the exact version
above, uses strict config, scrubs inherited OpenAI API-key/endpoint variables so
subscription auth wins, and gives actionable missing-CLI/auth diagnostics.

One Codex-side limitation remains explicit: app-server accepts per-MCP-server
`enabled_tools`, but this pin has no global native-tool allowlist matching the
frozen trait. The provider passes the requested boundary as developer
instructions and reports every observed native/MCP tool use, while relying on
the workspace sandbox for process isolation. Treat strict native-tool denial as
best-effort until Codex exposes a spawn-time control for it.
