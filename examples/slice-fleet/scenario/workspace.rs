//! Temporary copies of the bundled fixture used by workers and integration.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

const FIXTURE_MANIFEST: &str = include_str!("../fixture/Cargo.toml");
const FIXTURE_LOCK: &str = include_str!("../fixture/Cargo.lock");
const FIXTURE_LIB: &str = include_str!("../fixture/src/lib.rs");
const FIXTURE_INCREMENT: &str = include_str!("../fixture/src/increment.rs");
const FIXTURE_DOUBLE: &str = include_str!("../fixture/src/double.rs");
pub(super) const FIX_INCREMENT: &str = include_str!("../fixture/fixes/increment.rs");
pub(super) const FIX_DOUBLE: &str = include_str!("../fixture/fixes/double.rs");

#[derive(Debug)]
pub(super) struct TempFixture {
    root: PathBuf,
}

impl TempFixture {
    pub(super) fn new() -> Result<Self> {
        let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "odori-slice-fleet-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&root).with_context(|| format!("create {}", root.display()))?;
        let fixture = Self { root };
        for copy in [
            "snapshot",
            "increment-bugfix",
            "double-feature",
            "integrated",
        ] {
            fixture.seed(copy)?;
        }
        Ok(fixture)
    }

    fn seed(&self, copy: &str) -> Result<()> {
        let root = self.root.join(copy);
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("Cargo.toml"), FIXTURE_MANIFEST)?;
        fs::write(root.join("Cargo.lock"), FIXTURE_LOCK)?;
        fs::write(root.join("src/lib.rs"), FIXTURE_LIB)?;
        fs::write(root.join("src/increment.rs"), FIXTURE_INCREMENT)?;
        fs::write(root.join("src/double.rs"), FIXTURE_DOUBLE)?;
        Ok(())
    }

    pub(super) fn copy(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn run_id(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_TEMP.fetch_add(1, Ordering::Relaxed))
}

pub(super) fn is_scoped_path(path: &Path) -> bool {
    use std::path::Component;

    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}
