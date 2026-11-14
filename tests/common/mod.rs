//! Shared helpers for the end-to-end integration tests.
//!
//! Unique directories under the OS temp dir, removed on drop, plus the
//! backdating trick used to make TTLs behave without sleeping.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A throwaway root directory for one test.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    /// Create a fresh directory tagged with the test name.
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cachegirl-e2e-{tag}-{:x}-{}",
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { path: dir }
    }

    /// Path of the root.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Move `id`'s timestamps `secs` into the past, as if it had been
/// sitting around that long. The way to test TTL expiry without
/// sleeping.
pub fn backdate(manager: &cachegirl::TempManager, id: &cachegirl::ResourceId, secs: u64) {
    let past = SystemTime::now() - Duration::from_secs(secs);
    manager
        .registry()
        .update(id, |m| {
            m.created_at = past;
            m.last_accessed = past;
        })
        .unwrap();
}
