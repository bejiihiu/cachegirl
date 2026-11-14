//! Shared test helpers: unique temp dirs without external crates.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory that cleans itself up on drop.
pub(crate) struct TestDir(PathBuf);

impl TestDir {
    /// Create a fresh unique directory under the system temp dir.
    pub(crate) fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cachegirl-test-{nanos:x}-{tick:x}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("test dir must be creatable");
        Self(dir)
    }

    /// Path to the directory.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
