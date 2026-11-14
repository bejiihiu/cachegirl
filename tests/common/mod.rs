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

/// Hard two-way agreement between the registry and the on-disk layout.
///
/// Every file or directory directly inside a namespace directory must be
/// registered, and every registered filesystem resource must exist on
/// disk. The instance lock at the instance root is ignored; anything
/// else outside a namespace directory is reported as an orphan. Content
/// nested inside a directory resource is the application's business and
/// is not tracked here.
pub fn assert_disk_matches_registry(manager: &cachegirl::TempManager) {
    use std::collections::HashSet;
    use std::path::PathBuf;

    let metas = manager
        .registry()
        .snapshot()
        .expect("registry must be readable for the consistency check");
    let canon = |p: &PathBuf| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
    let registered: HashSet<PathBuf> = metas
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                cachegirl::ResourceKind::File | cachegirl::ResourceKind::Directory
            )
        })
        .filter_map(|m| m.metadata.get("path").map(PathBuf::from))
        .map(|p| canon(&p))
        .collect();

    let instance_dir = manager.root().join(manager.instance_id());
    let instance_dir =
        std::fs::canonicalize(&instance_dir).unwrap_or_else(|_| instance_dir.clone());
    let mut on_disk: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&instance_dir)
        .unwrap_or_else(|e| panic!("instance dir {instance_dir:?} unreadable: {e}"))
        .flatten()
    {
        let name = entry.file_name();
        if name == ".cachegirl.lock" {
            continue;
        }
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            for child in std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("namespace dir {path:?} unreadable: {e}"))
                .flatten()
            {
                on_disk.push(child.path());
            }
        } else {
            panic!("unexpected entry at the instance root: {path:?}");
        }
    }

    for path in &on_disk {
        let canonical = canon(path);
        assert!(
            registered.contains(&canonical),
            "file on disk is unknown to the registry: {canonical:?}"
        );
    }
    for path in &registered {
        assert!(
            path.exists(),
            "registry knows {path:?} but the disk disagrees"
        );
    }
}
