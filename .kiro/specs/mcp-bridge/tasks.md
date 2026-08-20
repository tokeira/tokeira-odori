# Implementation Plan

Slice O6, day 25+. Prerequisites: O2 primitives merged (run-loop workflow,
`Tool`, frozen provider trait); O3 Claude provider merged or co-landing;
`proptest` dev-dependency added under the dependency single-writer rule.
Q1–Q7 (requirements, Iteration Notes) that remain open at implementation
start are resolved by their draft answers, marked in code with a spec
reference.

- [ ] 1. Invocation registry (`odori-agents`, workflow-pure)
  - [ ] 1.1 Registry types and admission state machine
    - `InvocationId`, `InvocationState`, `InvocationRegistry` with
      `admit`/`complete`; deterministic, no I/O, replay-safe. Validation per
      the update-payload contract-policy table.
    - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - [ ] 1.2 Attempt fencing in admission
    - Supersession tracking per turn; `Fenced` admission for unrecorded
      stale-attempt calls; recorded-result serving for stale reads.
    - _Requirements: 4.1, 4.2, 4.3_
  - [ ] 1.3 `tool_invoked` update handler on the run-loop workflow
    - Payload validation → `admit` → schedule `execute_tool` with the
      per-`Tool` retry policy → await → `complete` → complete the update
      with the result. Run-to-completion policy for in-flight activities on
      harness death (Q6 draft).
    - _Requirements: 2.1, 2.2, 2.3, 6.2, 6.6_

- [ ] 2. Checkpoint: `odori-agents` compiles, clippy clean, registry unit
      tests green
  - `cargo clippy --workspace --all-targets --locked -- -D warnings` and
    `cargo nextest run --workspace --locked`.

- [ ] 3. Property test: Property 1 — at-most-once execution per identity
  - Reference-model PBT over generated presentation sequences (retries,
    resumes, later attempts), ≥100 iterations, in `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 1: at-most-once execution per identity`
  - _Requirements: 3.1, 3.2, 3.3, 7.4_

- [ ] 4. Property test: Property 3 — registry replay equivalence
  - Generate runs, truncate at arbitrary crash points, replay, compare
    registry state to the pre-crash model; ≥100 iterations, `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 3: registry replay equivalence`
  - _Requirements: 3.4, 7.2_

- [ ] 5. Property test: Property 4 — fencing
  - Generated interleavings of current/superseded-attempt calls; assert no
    `Execute` from a superseded attempt; ≥100 iterations, `odori-agents`.
  - Tag: `// Feature: mcp-bridge, Property 4: fencing`
  - _Requirements: 4.1, 4.2, 4.3_

- [ ] 6. Bridge server core (`odori-mcp-bridge`, behind `preview`)
  - [ ] 6.1 Streamable-HTTP MCP server on loopback
    - `127.0.0.1:0` listener, per-run bearer token check before any MCP
      processing, `initialize`/`ping`/`tools/list`, `method_not_found` for
      unserved surfaces per the contract-policy table.
    - _Requirements: 1.1, 1.2, 1.3, 1.7_
  - [ ] 6.2 `tools/call` → workflow update client
    - Build `tool_invoked` payloads; map update outcomes to MCP responses
      per the error-handling table (tool exhaustion → `isError: true`;
      update transport failure → `internal_error`).
    - _Requirements: 2.1, 2.3, 6.1, 6.3_
  - [ ] 6.3 Keepalive scheduler
    - Progress notifications below the pinned harness timeout; heartbeat
      passthrough when `execute_tool` heartbeats, synthesized otherwise.
    - _Requirements: 5.1, 5.2_

- [ ] 7. Checkpoint: workspace compiles with and without `preview`, clippy
      clean, bridge unit tests green
  - Feature-matrix build: `cargo clippy -p odori-mcp-bridge --locked` with
    `--features preview` and without.

- [ ] 8. Property test: Property 2 — record before respond
  - Bridge core against a fake update client with controllable completion
    ordering; ≥100 iterations, `odori-mcp-bridge`.
  - Tag: `// Feature: mcp-bridge, Property 2: record before respond`
  - _Requirements: 2.3_

- [ ] 9. Property test: Property 5 — keepalive cadence bound
  - Virtual-time PBT over generated (T, H, k) and heartbeat patterns;
    ≥100 iterations, `odori-mcp-bridge`.
  - Tag: `// Feature: mcp-bridge, Property 5: keepalive cadence bound`
  - _Requirements: 5.1, 5.2, 5.3_

- [ ] 10. Provider attachment (`odori-providers`)
  - [ ] 10.1 Claude spawn attachment
    - `--mcp-config` injection (server name per Q7), `--allowedTools`
      scoping, MCP timeout pinning at spawn.
    - _Requirements: 1.4, 1.5, 5.3_
  - [ ] 10.2 Codex session attachment
    - `mcp_servers` config on app-server session start; stdio re-exec shim
      if Q1 resolves to the fallback.
    - _Requirements: 1.6_
  - [ ] 10.3 Exit-classification extension
    - "Died awaiting MCP" folded into the O3 4-tuple; retryable turn
      failure; fork-vs-resume selection on retry per taxonomy.
    - _Requirements: 6.4, 6.5, 7.1, 7.3_

- [ ] 11. Property test: Property 6 — failure classes are preserved
  - Generated failure injections (tool exhaustion, update fault, harness
    death) against the taxonomy mapping; ≥100 iterations, spanning
    `odori-mcp-bridge` + `odori-providers`.
  - Tag: `// Feature: mcp-bridge, Property 6: failure classes are preserved`
  - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 6.6_

- [ ] 12. Wiring and the `preview` boundary (`odori-engine`, `odori` facade)
  - [ ] 12.1 `execute_tool` worker registration + idempotency context
    - Register on the embedded worker; inject run id/turn/attempt/invocation
      id into the tool context.
    - _Requirements: 2.2, 2.4_
  - [ ] 12.2 Facade wiring and `preview` gating
    - Bridge start/injection only under `preview`; native-tool delegation
      path with `preview` off; `odori-agents` keeps zero compile-time
      dependency on the bridge crate.
    - _Requirements: 8.1, 8.2, 8.3, 8.4, 8.5_

- [ ] 13. Property test: Property 7 — `preview`-off inertness
  - Facade-level test compiled under both feature configurations asserting
    no listener, no injected config, no bridge path; the `Tool` program
    compiles unchanged.
  - Tag: `// Feature: mcp-bridge, Property 7: preview-off inertness`
  - _Requirements: 8.1, 8.2, 8.3, 8.4_

- [ ] 14. Integration
  - [ ] 14.1 Scripted-harness crash-mid-turn recovery test
    - Deterministic fake harness against the embedded engine: kill at
      generated points (tool running, response unsent), assert §Recovery
      semantics end-to-end (resume, registry hits, fencing of stragglers).
    - _Requirements: 7.1, 7.2, 7.4, 3.1, 3.2, 3.3, 4.2_
  - [ ] 14.2 Live-harness bridged turn (both providers, pinned versions)
    - One bridged tool call end-to-end per harness; ignored-by-default
      (subscription quota); part of the launch dry-run checklist.
    - _Requirements: 1.4, 1.5, 1.6, 1.7, 2.3_

- [ ] 15. Checkpoint: full finish bar green
  - `cargo +nightly fmt --all`; clippy `-D warnings`; nextest; doctests;
    `RUSTDOCFLAGS="-D warnings" cargo doc`; `cargo deny check bans licenses
    sources` (proptest movement lands here).

## Task Dependency Graph

- 1 → 2 → {3, 4, 5}
- 1 → 6 → 7 → {8, 9}
- {1, 6} → 10 → 11
- {6, 10} → 12 → 13
- {3, 4, 5, 8, 9, 11, 13} → 14 → 15
- External: O2 merged before 1; O3 merged before 10; engine-repo T2 available
  before 12/14; Q1 answered before 10.2 commits to a transport.

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
