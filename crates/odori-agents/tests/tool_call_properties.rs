//! Property suites for the tool-call tracker , run against a reference model over generated
//! presentation/completion interleavings.

use std::collections::{HashMap, HashSet, VecDeque};

use odori_agents::tool_call::{ToolCallAdmission, ToolCallId, ToolCallResult, ToolCallTracker};
use proptest::prelude::*;

/// One step of a generated run: a presentation, or completing the oldest
/// outstanding execution (creating in-flight windows for joins).
#[derive(Debug, Clone)]
enum Op {
    Present { turn: u32, attempt: u32, call: u8 },
    CompleteOldest,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0u32..3, 1u32..5, 0u8..6).prop_map(|(turn, attempt, call)| Op::Present {
            turn,
            attempt,
            call,
        }),
        2 => Just(Op::CompleteOldest),
    ]
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(op_strategy(), 1..120)
}

/// Deterministic result for an identity, so replays can assert
/// byte-identical service.
fn canonical_result(turn: u32, call: u8) -> ToolCallResult {
    ToolCallResult::text(format!("result-{turn}-{call}"))
}

fn present(registry: &mut ToolCallTracker, turn: u32, attempt: u32, call: u8) -> ToolCallAdmission {
    let id = ToolCallId {
        turn,
        attempt,
        call_id: format!("call-{call}"),
    };
    registry.admit(&id, "tool")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // at-most-once execution per identity
    //
    // For any presentation sequence — retries, later attempts, interleaved
    // completions — at most one Execute is admitted per (turn, call id),
    // and every Recorded service returns the single execution's result
    // byte-identically.
    #[test]
    fn p1_at_most_once_execution_per_identity(ops in ops()) {
        let mut registry = ToolCallTracker::new();
        let mut executed: HashSet<(u32, u8)> = HashSet::new();
        let mut outstanding: VecDeque<((u32, u8), odori_agents::tool_call::ToolCallTicket)> =
            VecDeque::new();

        for op in ops {
            match op {
                Op::Present { turn, attempt, call } => {
                    match present(&mut registry, turn, attempt, call) {
                        ToolCallAdmission::Execute(ticket) => {
                            prop_assert!(
                                executed.insert((turn, call)),
                                "second Execute admitted for ({turn}, call-{call})"
                            );
                            outstanding.push_back(((turn, call), ticket));
                        }
                        ToolCallAdmission::Recorded(result) => {
                            prop_assert_eq!(
                                result,
                                canonical_result(turn, call),
                                "recorded service diverged from the single execution"
                            );
                        }
                        ToolCallAdmission::AwaitExisting | ToolCallAdmission::Fenced => {}
                    }
                }
                Op::CompleteOldest => {
                    if let Some(((turn, call), ticket)) = outstanding.pop_front() {
                        registry.complete(ticket, canonical_result(turn, call));
                    }
                }
            }
        }
    }

    // registry replay equivalence
    //
    // The registry is a deterministic fold of its admission history: a
    // fresh registry replaying the same operations from any crash point
    // reaches state equal to the original — with no contribution from
    // anything outside the recorded operations.
    #[test]
    fn p3_registry_replay_equivalence(ops in ops(), split in 0usize..120) {
        fn run(ops: &[Op]) -> ToolCallTracker {
            let mut registry = ToolCallTracker::new();
            let mut outstanding = VecDeque::new();
            for op in ops {
                match *op {
                    Op::Present { turn, attempt, call } => {
                        if let ToolCallAdmission::Execute(ticket) =
                            present(&mut registry, turn, attempt, call)
                        {
                            outstanding.push_back(((turn, call), ticket));
                        }
                    }
                    Op::CompleteOldest => {
                        if let Some(((turn, call), ticket)) = outstanding.pop_front() {
                            registry.complete(ticket, canonical_result(turn, call));
                        }
                    }
                }
            }
            registry
        }

        let original = run(&ops);
        let replayed = run(&ops);
        prop_assert_eq!(&original, &replayed, "same history, different state");

        // Crash at an arbitrary prefix boundary: the prefix state is itself
        // a valid fold (what a recovering workflow holds mid-history), and
        // continuing the suffix on it converges with the full fold. The
        // in-flight ticket queue is harness bookkeeping; state equality is
        // what replay must guarantee.
        let split = split.min(ops.len());
        let prefix_then_suffix = {
            let mut registry = ToolCallTracker::new();
            let mut outstanding = VecDeque::new();
            for op in &ops[..split] {
                match *op {
                    Op::Present { turn, attempt, call } => {
                        if let ToolCallAdmission::Execute(ticket) =
                            present(&mut registry, turn, attempt, call)
                        {
                            outstanding.push_back(((turn, call), ticket));
                        }
                    }
                    Op::CompleteOldest => {
                        if let Some(((turn, call), ticket)) = outstanding.pop_front() {
                            registry.complete(ticket, canonical_result(turn, call));
                        }
                    }
                }
            }
            for op in &ops[split..] {
                match *op {
                    Op::Present { turn, attempt, call } => {
                        if let ToolCallAdmission::Execute(ticket) =
                            present(&mut registry, turn, attempt, call)
                        {
                            outstanding.push_back(((turn, call), ticket));
                        }
                    }
                    Op::CompleteOldest => {
                        if let Some(((turn, call), ticket)) = outstanding.pop_front() {
                            registry.complete(ticket, canonical_result(turn, call));
                        }
                    }
                }
            }
            registry
        };
        prop_assert_eq!(&original, &prefix_then_suffix);
    }

    // fencing
    //
    // For any interleaving, no unrecorded call stamped with an attempt
    // below its turn's watermark is admitted to execute; superseded calls
    // are served from the registry where recorded and fenced otherwise.
    #[test]
    fn p4_fencing(ops in ops()) {
        let mut registry = ToolCallTracker::new();
        let mut watermark: HashMap<u32, u32> = HashMap::new();
        let mut recorded_or_inflight: HashSet<(u32, u8)> = HashSet::new();
        let mut outstanding = VecDeque::new();

        for op in ops {
            match op {
                Op::Present { turn, attempt, call } => {
                    let superseded = attempt < watermark.get(&turn).copied().unwrap_or(0);
                    let known = recorded_or_inflight.contains(&(turn, call));
                    let admission = present(&mut registry, turn, attempt, call);
                    if superseded && !known {
                        prop_assert!(
                            matches!(admission, ToolCallAdmission::Fenced),
                            "superseded unrecorded call was not fenced ({turn},{attempt},{call})"
                        );
                    }
                    if superseded && known {
                        prop_assert!(
                            matches!(
                                admission,
                                ToolCallAdmission::Recorded(_) | ToolCallAdmission::AwaitExisting
                            ),
                            "superseded known call must be served, not re-executed"
                        );
                    }
                    if let ToolCallAdmission::Execute(ticket) = admission {
                        prop_assert!(!superseded, "fence breached: stale attempt executed");
                        recorded_or_inflight.insert((turn, call));
                        outstanding.push_back(((turn, call), ticket));
                    }
                    let entry = watermark.entry(turn).or_insert(0);
                    *entry = (*entry).max(attempt);
                }
                Op::CompleteOldest => {
                    if let Some(((turn, call), ticket)) = outstanding.pop_front() {
                        registry.complete(ticket, canonical_result(turn, call));
                    }
                }
            }
        }
    }
}
