//! The persistent fixture workspace and its on-disk evidence.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};

pub(super) const APPROVAL_REQUEST: &str = "approval-request.json";
pub(super) const SNAPSHOT_FILE: &str = "engine.snapshot";
pub(super) const WORKSPACE: &str = "workspace";
pub(super) const ALLOWED_PATH: &str = "src/lib.rs";

const FIXTURE_MANIFEST: &str = include_str!("../fixture/Cargo.toml");
const FIXTURE_LOCK: &str = include_str!("../fixture/Cargo.lock");
pub(super) const BROKEN_LIB: &str = include_str!("../fixture/src/lib.rs");
pub(super) const FIXED_LIB: &str = include_str!("../fixture/fixed/lib.rs");

pub(super) fn seed(state_directory: &Path) -> Result<PathBuf> {
    ensure!(
        !state_directory.exists(),
        "state directory {} already exists; choose a new path",
        state_directory.display()
    );
    let workspace = state_directory.join(WORKSPACE);
    fs::create_dir_all(workspace.join("src"))?;
    fs::write(workspace.join("Cargo.toml"), FIXTURE_MANIFEST)?;
    fs::write(workspace.join("Cargo.lock"), FIXTURE_LOCK)?;
    fs::write(workspace.join(ALLOWED_PATH), BROKEN_LIB)?;
    Ok(workspace)
}

pub(super) fn test_succeeds(workspace: &Path) -> Result<bool> {
    let output = Command::new("cargo")
        .args(["test", "--locked"])
        .current_dir(workspace)
        .output()
        .context("run fixture test")?;
    Ok(output.status.success())
}
