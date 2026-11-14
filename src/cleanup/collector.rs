//! The cleanup pass itself: find reclaimable resources, release them.

use std::sync::Arc;
use std::time::Instant;

use crate::cleanup::report::CleanupReport;
use crate::core::error::Error;
use crate::namespace::NamespaceShared;
use crate::ownership::{DestroyOutcome, destroy_resource};

/// Reclaims resources whose lifecycle says they are done.
///
/// Operates on the registry plus the manager's backends. Resources
/// whose state is `Expired` or `PendingCleanup` are destroyed through
/// their backend; the registry record is claimed with `remove`, so a
/// concurrent release wins exactly one of the two calls. There is no
/// background garbage collector — call this whenever it makes sense.
///
/// **Leases win over cleanup.** A resource with at least one active
/// lease is never destroyed: it is moved to `PendingCleanup` (if it is
/// not there already) and counted as `deferred` in the report. It is
/// reclaimed once the last lease is gone.
pub struct CleanupEngine {
    shared: Arc<NamespaceShared>,
}

impl CleanupEngine {
    // construction goes through the manager; the engine needs the whole plumbing
    pub(crate) fn new(shared: Arc<NamespaceShared>) -> Self {
        Self { shared }
    }

    /// Reclaim everything in `Expired` or `PendingCleanup` state.
    ///
    /// One failing resource never stops the pass; the failure is
    /// recorded in the report and the pass continues.
    pub fn cleanup(&self) -> Result<CleanupReport, Error> {
        let started = Instant::now();
        let mut report = CleanupReport::default();
        for meta in self.shared.registry.snapshot()? {
            report.scanned += 1;
            if !meta.state.is_cleanup_eligible() {
                report.skipped += 1;
                continue;
            }
            match destroy_resource(&self.shared, &meta.id) {
                Ok(DestroyOutcome::Destroyed) => {
                    report.removed += 1;
                    report.bytes_freed = report.bytes_freed.saturating_add(meta.size_bytes);
                }
                Ok(DestroyOutcome::AlreadyGone) => {
                    report.skipped += 1;
                }
                Ok(DestroyOutcome::Deferred) => {
                    report.deferred += 1;
                }
                Err(e) => {
                    report.failed += 1;
                    report.failure_reasons.push(format!("{}: {e}", meta.id));
                }
            }
        }
        report.duration = started.elapsed();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lifecycle::ResourceState;
    use crate::core::resource::{ResourceKind, ResourceMeta};

    #[test]
    fn reclaims_only_eligible_resources() {
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        let mut expired = ResourceMeta::new("gone", "ns", ResourceKind::File);
        expired.size_bytes = 42;
        expired.state = ResourceState::Expired;
        // the physical side was never materialized; backend release must tolerate that
        let active = ResourceMeta::new("live", "ns", ResourceKind::File);
        shared.registry.register(expired).unwrap();
        shared.registry.register(active).unwrap();

        let report = engine.cleanup().unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.removed, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.bytes_freed, 42);
        assert_eq!(shared.registry.len().unwrap(), 1);
    }

    #[test]
    fn leased_resource_is_deferred_not_destroyed() {
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        let mut pending = ResourceMeta::new("busy", "ns", ResourceKind::File);
        pending.state = ResourceState::PendingCleanup;
        shared.registry.register(pending.clone()).unwrap();

        let lease = shared.leases.acquire(&pending.id).unwrap();
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.deferred, 1);
        // record survives, marked pending
        let meta = shared.registry.get(&pending.id).unwrap();
        assert_eq!(meta.state, ResourceState::PendingCleanup);

        drop(lease);
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(report.deferred, 0);
        assert!(shared.registry.get(&pending.id).is_err());
    }
}
