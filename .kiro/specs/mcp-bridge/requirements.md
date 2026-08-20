# Requirements Document

## Introduction

The MCP bridge makes framework-owned tools durable when a vendor harness calls
them mid-turn. Odori's execution model is: the run loop is a workflow, one
harness turn is one activity. A `Tool` registered on an `Agent` must execute as
a tokeira activity — with its own retry policy, recorded in the run's history —
while the harness subprocess blocks awaiting the MCP response inside a turn
that is itself an activity.

Everything hard here follows from one fact: **the harness is not
replay-aware**. Tokeira replays workflow history deterministically; the harness
replays its *own* session history on resume and re-issues work. The bridge is
where the two replay models meet, and its job is to make the meeting
idempotent.

This spec is **foundational** (it fixes workflow state shape and the provider
spawn contract) and was produced **design-first**: `design.md` carries the
architecture this document formalises. Scope boundary: the bridge and its
workflow/provider seams. The turn loop itself (slice O2), the providers' turn
supervision (O3/O4), and the embedded engine assembly (engine-repo slice T2)
are sibling work this spec consumes. Implementation is slice O6 (day 25); this
spec freezes first. Ships behind the `preview` feature (descope ladder rung 3).

## Glossary

- **Harness:** a vendor agent CLI driven as a supervised subprocess — headless
  Claude Code (`claude -p --output-format stream-json`) or the Codex
  app-server.
- **Turn:** one harness invocation within a run; executed as one tokeira
  activity ("the turn activity") by a provider.
- **Attempt:** one execution of a turn activity. Attempts are monotonic per
  turn; a retry produces attempt N+1 and supersedes attempt N.
- **Call id:** the harness-assigned tool-use id for one tool call. It is
  persisted in the harness's own session history, so a resumed session
  re-issues a pending call with the same id.
- **Invocation identity:** the triple (turn, attempt, call id) stamped on
  every bridged tool call. The registry keys on (turn, call id); the attempt
  is a fencing dimension, not an identity dimension.
- **Registry:** the run-loop workflow's record of tool invocations — in
  flight or complete, with results. Workflow state only; rebuilt by replay.
- **Bridge:** the in-process MCP server plus its workflow-update client.
  Executes no tools itself; holds no durable state.
- **Update:** tokeira's synchronous workflow update — request/response into a
  workflow, completing only after the handler's effects are in history.
- **`execute_tool`:** the activity that runs one framework tool, scheduled by
  the run-loop workflow with the per-`Tool` retry policy.
- **Keepalive:** an MCP progress notification emitted while a call is
  pending, resetting a compliant MCP client's timeout.
- **Superseded attempt:** any attempt older than the newest the workflow has
  scheduled for that turn. Its calls are "stale".
- **`preview`:** the feature flag on `odori-mcp-bridge` gating the whole
  bridge.

## Target State

With `preview` enabled: every framework-tool call from either harness executes
as exactly one recorded tokeira activity per invocation identity — across MCP
client retries, harness resumes, turn-activity retries, and whole-process
crashes — with results the harness only ever observes after they are in
history. Tool failure, bridge failure, and harness death remain distinct
failure classes. With `preview` disabled: the bridge is inert (no listener, no
config injection) and `Tool` intent delegates to the harness's native tooling,
with the turn as the durability boundary.

Out of scope: the bridge exposing harness-*native* tools to the framework;
resources/prompts/sampling MCP surfaces (tools only in v0); cross-run tool
result sharing; the O2 turn loop and O3/O4 supervision internals.

Sanctioned exception: the **regenerated-id residual** — if a tool executed but
the harness died before persisting the `tool_use` block, the retried attempt
regenerates the call under a fresh id no key can connect to the first
execution. The tool runs again. No bridge design closes this without vendor
cooperation (the id is born inside the harness); the contract is that
framework tools are idempotent or compensatable, using the injected
idempotency key (Requirement 2.4). See design §"Idempotency" and open
question Q3.

## Evidence From Current Code

- **Execution model (authoritative):** launch plan "Twelve Days to Public"
  (scope contract + slice table) — run-loop-as-workflow / turn-as-activity,
  update/signal shipping in v0, `preview` as descope rung 3.
- **Harness behaviour (observed):** `spikes/claude-driver/README.md` —
  claude 2.1.220 ground truth: `--mcp-config`, `--allowedTools`, `--resume`,
  `--fork-session` present; stream events as liveness; failure classification
  4-tuple (exit code, result-event-arrived, `terminal_reason`, stderr); spawn
  env scrubbing.
- **Current code:** skeleton module docs only — `crates/odori-mcp-bridge/src/lib.rs`
  (bridge charter, `preview` flag), `crates/odori-agents/src/lib.rs` (runner
  charter), `crates/odori-providers/src/lib.rs` (provider tiers),
  `crates/odori-engine/src/lib.rs` (worker bootstrap). No behaviour exists
  yet; this spec is green-field against those charters.
- **Dependencies:** O2 primitives (runner workflow this spec's update handler
  and registry live in; provider trait frozen EOD day 22); O3/O4 providers
  (spawn-time attachment); engine-repo T2 (`Engine::embedded()`); Temporal
  Rust SDK 0.7 update support (workspace pin, `temporalio-sdk = "0.7"`).
- **Unverified assumptions, tracked:** call-id stability across session
  resume per harness (Q2 — claude probe auth-blocked; Codex probe is lane
  C's); Codex HTTP-MCP support (Q1).

## Contract Policy

### MCP server surface (methods the bridge answers)

| Element | Target policy | Error if invalid | Persistence/side-effect impact |
|---|---|---|---|
| `initialize` | Answer with tools capability only | — | none |
| `ping` | Answer | — | none |
| `tools/list` | Exactly the agent's `Tool` set for the current turn, stable within the turn | — | none |
| `tools/call` | Translate to workflow update per Requirements 2–4 | Unknown tool name → MCP `invalid_params`; missing/invalid bearer token → auth error before any registry consult | one registry entry + at most one `execute_tool` lineage |
| `notifications/progress` (emitted) | Sent per Requirement 5 while a call is pending | — | none |
| resources/prompts/sampling/completion methods | Not served in v0 | MCP `method_not_found` | none |

### Update payload (`tool_invoked`, bridge → run-loop workflow)

| Field | Target policy | Error if invalid | Persistence/side-effect impact |
|---|---|---|---|
| `turn` | Must name the workflow's current turn | Unknown turn → rejected, fencing error | registry key part |
| `attempt` | Fencing token; compared to newest scheduled attempt | Superseded + unrecorded call → rejected (Req 4.2) | never persisted as identity; logged |
| `call_id` | Harness tool-use id, opaque, non-empty | Empty → rejected, `invalid_params` | registry key part |
| `tool_name` | Must be in the turn's `Tool` set | Unknown → rejected, `invalid_params` | recorded on the entry |
| `arguments` | JSON, validated against the `Tool`'s schema before scheduling | Schema-invalid → MCP tool result `isError: true` (model-visible, not a bridge failure) | recorded on the entry |

## Requirements

### Requirement 1: In-process MCP server and harness attachment

**User Story:** As a framework author, I want each harness pointed at one
in-process MCP endpoint that only my spawned harnesses can call, so that
framework tools are reachable mid-turn without a separate server to operate.

#### Acceptance Criteria

1. WHERE `preview` is enabled, THE engine SHALL run exactly one bridge MCP
   server per process, listening with streamable HTTP on `127.0.0.1` at an
   ephemeral port.
2. WHEN a run starts, THE bridge SHALL mint a per-run bearer token and expose
   it only via the harness MCP configuration the provider injects.
3. IF an MCP request does not carry the run's bearer token, THEN THE bridge
   SHALL reject it before consulting the registry.
4. WHEN the Claude provider spawns a turn, THE provider SHALL attach the
   bridge via `--mcp-config` declaring one HTTP server named `odori`.
5. WHEN the Claude provider spawns a turn, THE provider SHALL scope
   `--allowedTools` to the bridge's tool set.
6. WHEN the Codex provider starts an app-server session, THE provider SHALL
   attach the bridge via `mcp_servers` configuration — directly over HTTP
   where the pinned Codex supports it, otherwise through the stdio re-exec
   shim (Q1).
7. WHEN the harness issues `tools/list`, THE bridge SHALL return exactly the
   agent's `Tool` set for the current turn.

### Requirement 2: Tool call to durable activity

**User Story:** As a tool author, I want every bridged invocation to run as a
tokeira activity with my declared retry policy, so that tool effects are
durable, retried, and inspectable in run history.

#### Acceptance Criteria

1. WHEN the bridge receives `tools/call`, THE bridge SHALL submit a workflow
   update carrying the invocation identity, tool name, and arguments.
2. WHEN the update handler admits a fresh invocation, THE run-loop workflow
   SHALL schedule `execute_tool` with that `Tool`'s retry policy and
   timeouts.
3. THE bridge SHALL complete a `tools/call` response only after the
   corresponding update has completed.
4. WHEN `execute_tool` runs, THE engine SHALL supply the tool implementation
   with the run id, turn, attempt, and invocation id as its idempotency key.

### Requirement 3: Idempotent replay

**User Story:** As an operator, I want retried and resumed turns to reuse
already-executed tool results, so that recovery never double-executes a tool
whose identity is known.

#### Acceptance Criteria

1. WHEN `tools/call` presents a (turn, call id) recorded complete in the
   registry, THE run-loop workflow SHALL return the recorded result without
   scheduling a new activity.
2. WHILE an invocation is in flight, WHEN the same (turn, call id) is
   presented again, THE run-loop workflow SHALL join the existing execution
   and return its result.
3. WHEN a newer turn attempt re-presents a call id recorded by a prior
   attempt, THE run-loop workflow SHALL serve it from the registry.
4. THE registry SHALL live exclusively in run-loop workflow state.

### Requirement 4: Attempt fencing

**User Story:** As an operator, I want calls from zombie harnesses rejected,
so that a superseded attempt can never mutate the world after its successor
started.

#### Acceptance Criteria

1. WHEN the run-loop workflow schedules turn attempt N+1, THE run-loop
   workflow SHALL treat every attempt ≤ N of that turn as superseded.
2. IF a call stamped with a superseded attempt is not recorded in the
   registry, THEN THE run-loop workflow SHALL reject it without scheduling
   work.
3. WHEN a call stamped with a superseded attempt presents a recorded (turn,
   call id), THE run-loop workflow SHALL return the recorded result.

### Requirement 5: Timeout interplay and keepalive

**User Story:** As a tool author, I want long-running tools to survive the
harness's MCP client timeout, so that a 10-minute deploy does not need the
model to poll.

#### Acceptance Criteria

1. WHILE a `tools/call` update is pending, THE bridge SHALL emit MCP progress
   notifications at a cadence strictly below the harness's configured MCP
   timeout.
2. WHERE `execute_tool` reports activity heartbeats, THE bridge SHALL derive
   progress notifications from them.
3. WHEN the provider spawns a harness, THE provider SHALL pin or set the
   harness's MCP timeout so the keepalive bound is known rather than guessed.
4. WHEN the runner derives the turn activity's default start-to-close
   timeout, THE runner SHALL make it at least the longest schedule-to-close
   among the agent's tools.

### Requirement 6: Failure taxonomy

**User Story:** As an agent author, I want tool failure, bridge failure, and
harness death to surface differently, so that the model adapts to tool
errors while infrastructure faults retry invisibly.

#### Acceptance Criteria

1. IF `execute_tool` exhausts its retry policy, THEN THE bridge SHALL return
   an MCP tool result with `isError: true`.
2. WHEN a tool activity exhausts its retry policy, THE run-loop workflow
   SHALL NOT fail the turn on that account.
3. IF the update path fails (engine unreachable, malformed frame,
   serialization error), THEN THE bridge SHALL return an MCP protocol error.
4. WHEN a bridge protocol error reaches the turn activity, THE turn activity
   SHALL fail as retryable.
5. WHEN the harness process exits while calls are in flight, THE turn
   activity SHALL fail as retryable, classified from the provider's exit
   4-tuple (exit code, result-event-arrived, `terminal_reason`, stderr).
6. WHEN the harness process exits while calls are in flight, THE run-loop
   workflow SHALL let in-flight `execute_tool` activities run to completion
   and record their results (draft policy — Q6).

### Requirement 7: Crash-mid-turn recovery

**User Story:** As an operator, I want a crash at any point in a turn — tool
running, response unsent, whole process down — to recover to exactly-once
observable behaviour, so that durability is real and not a demo claim.

#### Acceptance Criteria

1. WHEN a turn activity is retried after harness death or process restart,
   THE provider SHALL resume the harness session by its recorded session id.
2. WHEN the framework process restarts, THE run-loop workflow SHALL rebuild
   the registry — including in-flight entries — from history replay alone.
3. WHERE a turn retry follows a model-side failure (provider taxonomy
   `api_error`), THE provider SHALL resume with `--fork-session` so attempt
   histories stay isolated in the session lineage.
4. WHEN a resumed session re-issues a pending tool call, THE run-loop
   workflow SHALL satisfy it under Requirements 3.1–3.3 semantics.

### Requirement 8: The `preview` boundary

**User Story:** As a launch owner, I want the bridge cleanly severable behind
`preview`, so that descope ladder rung 3 is a flag flip, not a surgery.

#### Acceptance Criteria

1. WHERE `preview` is disabled, THE engine SHALL NOT start a bridge listener.
2. WHERE `preview` is disabled, THE provider SHALL NOT inject MCP
   configuration into the harness.
3. WHERE `preview` is disabled, THE runner SHALL delegate `Tool` intent to
   the harness's native tooling, with the turn as the durability boundary.
4. THE `Tool` API surface SHALL be identical with `preview` on and off.
5. THE `odori-agents` crate SHALL NOT depend on `odori-mcp-bridge` at compile
   time; the facade wires the bridge in.

## Iteration and Feedback Notes

Open questions held for Ian (numbering shared with `design.md`):

- **Q1 — Codex transport:** HTTP MCP directly, or commit the stdio shim from
  day one? (Lane C's Codex-driver PoC answers; the shim is the fallback
  either way.)
- **Q2 — Call-id stability on resume:** asserted from session-history
  mechanics; verify live per harness before freeze. Claude probe is blocked
  on the expired local CLI OAuth (PR #1 notes).
- **Q3 — Second-level dedupe:** accept the regenerated-id residual (current
  draft), or add args-hash heuristics with their wrong-dedupe risk?
- **Q4 — Result size policy:** tool results ride updates → history → MCP.
  Cap-and-fail, or offload past a threshold — and to where, embedded?
- **Q5 — Loopback auth depth:** per-run token (draft) or per-attempt tokens
  folded into fencing?
- **Q6 — Orphaned in-flight tools:** run-to-completion-and-record (draft,
  Requirement 6.6) or per-`Tool` cancel policy?
- **Q7 — Server naming:** `odori` (draft) is model-visible prompt surface;
  confirm.
