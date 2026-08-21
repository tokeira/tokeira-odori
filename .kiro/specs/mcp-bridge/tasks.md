# Implementation Plan

Slice O6, day 25+. Prerequisites: O2 primitives merged (run-loop workflow,
`Tool`, frozen provider trait); O3 Claude provider merged or co-landing;
`proptest` dev-dependency added under the dependency single-writer rule.
Q1–Q7 (requirements, Iteration Notes) that remain open at implementation
start are resolved by their draft answers, marked in code with a spec
reference.

- [x] 1. Invocation registry (`odori-agents`, workflow-pure)
  - [x] 1.1 Registry types and admission state machine
    - `InvocationId`, `InvocationState`, `InvocationRegistry` with
      `admit`/`complete`; deterministic, no I/O, replay-safe. Validation per
      the update-payload contract-policy table.
    - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - [x] 1.2 Attempt fencing in admission
    - Supersession tracking per turn; `Fenced` admission for unrecorded
      stale-attempt calls; recorded-result serving for stale reads.
    - _Requirements: 4.1, 4.2, 4.3_
  - [x] 1.3 `tool_invoked` update handler on the run-loop workflow
    - Payload validation → `admit` → schedule `execute_tool` with the
      per-`Tool` retry policy → await → `complete` → complete the update
      with the result. Run-to-completion policy for in-flight activities on
      harness death (Q6 draft).
    - _Requirements: 2.1, 2.2, 2.3, 6.2, 6.6_

- [x] 2. Checkpoint: `odori-agents` compiles, clippy clean, registry unit
      tests green
  - `cargo clippy --workspace --all-targets --locked -- -D warnings` and
    `cargo nextest run --workspace --locked`.

- [x] 3. Property test: Property 1 — at-most-once execution per identity
  - Reference-model PBT over generated presentation sequences (retries,
    resumes, later attempts), ≥100 iterations, in `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 1: at-most-once execution per identity`
  - _Requirements: 3.1, 3.2, 3.3, 7.4_

- [x] 4. Property test: Property 3 — registry replay equivalence
  - Generate runs, truncate at arbitrary crash points, replay, compare
    registry state to the pre-crash model; ≥100 iterations, `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 3: registry replay equivalence`
  - _Requirements: 3.4, 7.2_

- [x] 5. Property test: Property 4 — fencing
  - Generated interleavings of current/superseded-attempt calls; assert no
    `Execute` from a superseded attempt; ≥100 iterations, `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 4: fencing`
  - _Requirements: 4.1, 4.2, 4.3_

- [x] 6. Bridge server core (`odori-mcp-bridge`, behind `preview`)
  - [x] 6.1 Streamable-HTTP MCP server on loopback
    - `127.0.0.1:0` listener, per-run bearer token check before any MCP
      processing, `initialize`/`ping`/`tools/list`, `method_not_found` for
      unserved surfaces per the contract-policy table.
    - _Requirements: 1.1, 1.2, 1.3, 1.7_
  - [x] 6.2 `tools/call` → workflow update client
    - Build `tool_invoked` payloads; map update outcomes to MCP responses
      per the error-handling table (tool exhaustion → `isError: true`;
      update transport failure → `internal_error`).
    - _Requirements: 2.1, 2.3, 6.1, 6.3_
  - [x] 6.3 Keepalive scheduler
    - Progress notifications below the pinned harness timeout; heartbeat
      passthrough when `execute_tool` heartbeats, synthesized otherwise.
    - _Requirements: 5.1, 5.2_

- [x] 7. Checkpoint: workspace compiles with and without `preview`, clippy
      clean, bridge unit tests green
  - Feature-matrix build: `cargo clippy -p odori-mcp-bridge --locked` with
    `--features preview` and without.

- [x] 8. Property test: Property 2 — record before respond
  - Bridge core against a fake update client with controllable completion
    ordering; ≥100 iterations, `odori-mcp-bridge`.
  - Tag: `// Feature: mcp-bridge, Property 2: record before respond`
  - _Requirements: 2.3_

- [x] 9. Property test: Property 5 — keepalive cadence bound
  - Virtual-time PBT over generated (T, H, k) and heartbeat patterns;
    ≥100 iterations, `odori-mcp-bridge`.
  - Tag: `// Feature: mcp-bridge, Property 5: keepalive cadence bound`
  - _Requirements: 5.1, 5.2, 5.3_

- [ ] 10. Provider attachment (`odori-providers`)
  - [x] 10.1 Claude spawn attachment
    - `--mcp-config` injection (server name per Q7), `--allowedTools`
      scoping, MCP timeout pinning at spawn.
    - _Requirements: 1.4, 1.5, 5.3_
  - [ ] 10.2 Codex session attachment
    - `mcp_servers` config on app-server session start; stdio re-exec shim
      if Q1 resolves to the fallback.
    - _Requirements: 1.6_
  - [x] 10.3 Exit-classification extension
    - "Died awaiting MCP" folded into the O3 4-tuple; retryable turn
      failure; fork-vs-resume selection on retry per taxonomy.
    - _Requirements: 6.4, 6.5, 7.1, 7.3_

- [x] 11. Property test: Property 6 — failure classes are preserved
  - Generated failure injections (tool exhaustion, update fault, harness
    death) against the taxonomy mapping; ≥100 iterations, spanning
    `odori-mcp-bridge` + `odori-providers`.
  - Tag: `// Feature: mcp-bridge, Property 6: failure classes are preserved`
  - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 6.6_

- [x] 12. Wiring and the `preview` boundary (`odori-engine`, `odori` facade)
  - [x] 12.1 `execute_tool` worker registration + idempotency context
    - Register on the embedded worker; inject run id/turn/attempt/invocation
      id into the tool context.
    - _Requirements: 2.2, 2.4_
  - [x] 12.2 Facade wiring and `preview` gating
    - Bridge start/injection only under `preview`; native-tool delegation
      path with `preview` off; `odori-agents` keeps zero compile-time
      dependency on the bridge crate.
    - _Requirements: 8.1, 8.2, 8.3, 8.4, 8.5_

- [x] 13. Property test: Property 7 — `preview`-off inertness
  - Facade-level test compiled under both feature configurations asserting
    no listener, no injected config, no bridge path; the `Tool` program
    compiles unchanged.
  - Tag: `// Feature: mcp-bridge, Property 7: preview-off inertness`
  - _Requirements: 8.1, 8.2, 8.3, 8.4_

- [ ] 14. Integration
  - [x] 14.1 Scripted-harness crash-mid-turn recovery test
    - Deterministic fake harness against the embedded engine: kill at
      generated points (tool running, response unsent), assert §Recovery
      semantics end-to-end (resume, registry hits, fencing of stragglers).
    - _Requirements: 7.1, 7.2, 7.4, 3.1, 3.2, 3.3, 4.2_
  - [x] 14.2 Live-harness bridged turn (Claude leg; Codex waits on 10.2)
    - One bridged tool call end-to-end per harness; ignored-by-default
      (subscription quota); part of the launch dry-run checklist. Must
      also observe the real harness MCP client against the bridge's SSE
      responses — retry behaviour on timeout and whether a retried call
      re-presents the same `_meta` call id (the scripted harness covers
      only the protocol shape the spec commits to).
    - _Requirements: 1.4, 1.5, 1.6, 1.7, 2.3, 3.2, 5.1_

- [ ] 16. Token-directory eviction (`odori-mcp-bridge`)
  - The attachment directory retains one entry per turn-attempt for the
    process lifetime (deliberate while the run lives — stale tokens must
    resolve to be *fenced*, not 401). Evict a run's entries when its
    workflow reaches a terminal state; keep the fencing property (P4) and
    the fenced-not-401 behaviour under test.
  - _Requirements: 1.2, 1.3, 4.2_

- [x] 15. Checkpoint: full finish bar green
  - `cargo +nightly fmt --all`; clippy `-D warnings`; nextest; doctests;
    `RUSTDOCFLAGS="-D warnings" cargo doc`; `cargo deny check bans licenses
    sources` (proptest movement lands here).

## Task Dependency Graph

- 1 → 2 → {3, 4, 5}
- 1 → 6 → 7 → {8, 9}
- {1, 6} → 10 → 11
- {6, 10} → 12 → 13
- {3, 4, 5, 8, 9, 11, 13} → 14 → 15
- 6 → 16 (post-v0 acceptable; before the service tier's long-lived processes)
- External: O2 merged before 1; O3 merged before 10; engine-repo T2 available
  before 12/14; Q1 answered before 10.2 commits to a transport.

## Ledger notes (implementation, 2026-08-20)

Ticked tasks landed in the O6 implementation PR and the O3 provider PR.
Still open, with cause:

- **10.2 Codex attachment** — blocked on lane C's Q1 transport answers; the
  stdio shim already renders (`claude_flags` covers both transports). The
  Codex leg of 14.2 travels with it.
- **16 Token-directory eviction** — the O6 PR's residual, filed here rather
  than living only in the PR body.

O3 closures (2026-08-21): 10.3 landed as the additive
`TurnError::HarnessDiedAwaitingTools` variant, detected provider-side from
`tool_use` events no `tool_result` answered before death. 11 landed as a
256-case property suite over the provider's pure classification (the tool-
and bridge-failure legs were already proven in the O6 suites). 14.2's
Claude leg landed behind the ignored-by-default marker AND was run live
(claude 2.1.237, drifted from the 2.1.220 pin — protocol tolerance held;
drift warning fired as designed): real harness turn through the embedded
engine, and a real bridged `record_fact` call executed exactly once as a
durable activity with its receipt in the run's final answer. The
real-harness SSE observation from 14.2's scope: the CLI's MCP client
consumed the bridge's SSE `tools/call` responses cleanly end to end.

Decisions taken during implementation (mirrored in requirements.md):
**Q4** closed as 256 KiB cap-and-fail (`BridgeConfig::max_result_bytes`,
enforced at `execute_tool` before anything enters history); **Q5** closed
as per-attempt bearer tokens (fencing requires the wire attempt to be the
spawning attempt); fencing's "current attempt" is a per-turn watermark fed
by presented invocations (replay-deterministic).

## Notes

- The registry (task 1) is deliberately pure so P1/P3/P4 run as fast
  model-based PBTs without an engine in the loop; the update handler is the
  only place workflow APIs touch it.
- `proptest` enters the workspace as a dev-dependency in the first PBT task's
  PR — dependency movement, single-writer rule applies, `cargo deny` re-run
  in that PR (task 15 double-checks).
- Live-harness tests (14.2) stay out of the default bar: quota and
  auth-dependent. The scripted harness (14.1) is the durable regression net.
- The regenerated-id residual (requirements, Target State) is intentionally
  untested here: it is a documented contract on tool authors, not bridge
  behaviour; revisit if Q3 flips to heuristic dedupe.
