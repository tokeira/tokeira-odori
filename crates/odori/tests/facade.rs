//! Workspace smoke test: the facade's re-export surface stays wired.
//!
//! Seeds the test harness (the finish bar runs nextest from day one) and
//! fails to compile if a facade module is dropped or renamed.

#[test]
fn facade_reexports_every_workspace_crate() {
    // Module paths are the assertion; referencing them compiles only while
    // the facade re-exports all four crates under their stable names.
    use odori::{agents as _, engine as _, mcp_bridge as _, providers as _};
}
