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
        // nanos alone collide when several tests create their dir inside
        // the same clock tick — and then one test's drop wipes the other's
        // root from under it. an atomic counter removes the overlap
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tick = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cachegirl-e2e-{tag}-{:x}-{}-{}",
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            tick,
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
/// Every file anywhere under the instance directory must be registered,
/// and every registered filesystem resource must exist on disk. Nested
/// namespaces are walked recursively; directory resources (registered
/// with kind `Directory`) own their content, so their contents are not
/// tracked. The instance lock at the instance root is ignored; anything
/// else outside a namespace directory is reported as an orphan.
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
    let registered_dirs: HashSet<PathBuf> = metas
        .iter()
        .filter(|m| m.kind == cachegirl::ResourceKind::Directory)
        .filter_map(|m| m.metadata.get("path").map(PathBuf::from))
        .map(|p| canon(&p))
        .collect();

    let instance_dir = manager.root().join(manager.instance_id());
    let instance_dir =
        std::fs::canonicalize(&instance_dir).unwrap_or_else(|_| instance_dir.clone());
    let instance_root = instance_dir.clone();
    let mut on_disk: Vec<PathBuf> = Vec::new();
    let mut stack = vec![instance_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("instance dir {dir:?} unreadable: {e}"))
            .flatten()
        {
            let name = entry.file_name();
            if dir == instance_root && name == ".cachegirl.lock" {
                continue;
            }
            let path = entry.path();
            let is_dir = std::fs::symlink_metadata(&path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if is_dir {
                // a registered directory resource owns its content; a
                // namespace directory is just another level to descend
                if registered_dirs.contains(&canon(&path)) {
                    continue;
                }
                stack.push(path);
            } else {
                on_disk.push(path);
            }
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
