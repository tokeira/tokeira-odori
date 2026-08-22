# Durable tools

A framework tool can be called while a vendor harness is still inside its
turn. The optional MCP bridge carries that call back into the run-loop workflow
so the tool executes as its own durable activity.

## Enable the bridge

Enable `preview` on the `odori` dependency, register `Tool` values on the
agent, and pass a `BridgeConfig` to `OdoriRuntimeBuilder::bridge`. The
embedded examples' manifest demonstrates the feature wiring, and
[slice-fleet](../examples/slice-fleet/scenario/runtime.rs) demonstrates runtime
assembly.

With `preview` disabled, there is no listener and no harness MCP attachment.
A framework tool's Rust handler does not run; harness-native tools remain
controlled by `Agent::with_allowed_native_tools`.

## Call path

With `preview` enabled:

1. The runtime starts one streamable-HTTP MCP server on
   `127.0.0.1` at an ephemeral port.
2. Each turn attempt that has tools receives a new bearer token. Claude Code
   receives inline `--mcp-config`; Codex receives
   `mcp_servers.<name>.url` and `env_http_headers` at thread start, resume,
   or fork.
3. `tools/call` submits the invocation as a workflow update.
4. A fresh invocation schedules `execute_tool` with the tool's
   `ToolPolicy`.
5. The bridge replies only after the update has recorded the result in
   workflow history.

The default bridge keepalive is 10 seconds and the default harness MCP timeout
pin is 120 seconds. Configuration rejects a keepalive cadence that is not
strictly below the timeout. Keepalive progress prevents an idle transport from
looking dead; Codex 0.148.0-alpha.15 still applies a fixed wall-clock tool
timeout, so progress does not extend the maximum execution time.

## Replay and at-most-once admission

The durable registry keys an invocation by `(turn, call_id)`. A completed
identity returns its byte-identical recorded result. A duplicate received while
the first call is still running joins that execution. A stale turn attempt may
read a result already recorded under its call ID, but cannot admit new work.

This is at-most-once admission to an `execute_tool` activity lineage, not a
promise that arbitrary external side effects are transactional. Activity
retries may call a handler again after failure, and a harness may generate a
new call ID when it retries a logical request. The handler receives
`ToolContext { run_id, turn, attempt, invocation_id }`; use that identity in
the external system's idempotency mechanism.

Observed harness behavior matters:

- Claude Code preserves the tool-use ID for a duplicate presentation but may
  generate a fresh ID after process death and session resume.
- Codex 0.148.0-alpha.15 generated a fresh `_meta.callId` after
  process-death resume and when retrying a timed-out call in the same turn.

The bridge therefore deduplicates identities it can prove equal and does not
guess that different IDs mean the same side effect.

## Failure behavior

A terminal tool failure or exhausted tool-activity retry becomes an MCP
`isError` result that the model can read. An update-path fault is a protocol
failure and makes the containing turn retryable. If the harness dies while it
awaits a tool, the tool execution continues to completion and records its
result; a later presentation of the same identity replays it.

One result is capped at 256 KiB by default before it enters history. An
oversized result becomes a model-visible error instructing the tool to write
large output elsewhere and return a path. `BridgeConfig::max_result_bytes`
changes that ceiling.

Tokens are per turn attempt. Tokens from superseded attempts remain resolvable
while the workflow is live so stale calls reach the registry and are fenced,
not misreported as HTTP 401. All of a run's tokens are evicted after the
workflow is confirmed terminal.

## API providers as MCP clients

The API providers have no harness to host native tools. When an API-backed
agent declares framework tools, `preview` and a runtime bridge are required.
The provider lists tools from the bridge, maps them to the vendor's function
schema, and POSTs `tools/call` itself using the vendor tool-use ID.

Declaring tools without a bridge is a configuration error. Supplying
harness-native allowed tools to an API provider is also a configuration error.
The API path accepts the bridge's HTTP attachment; it cannot dial a stdio MCP
attachment.

