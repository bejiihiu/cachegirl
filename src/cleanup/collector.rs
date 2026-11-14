//! The cleanup pass itself: find reclaimable resources, release them.

use std::sync::Arc;
use std::time::Instant;

use crate::cleanup::report::CleanupReport;
use crate::core::error::RegistryError;
use crate::core::registry::Registry;

/// Reclaims resources whose lifecycle says they are done.
///
/// Operates purely on the registry; backends own the physical removal.
/// Call it whenever the application wants — there is no background GC.
pub struct CleanupEngine {
    registry: Arc<Registry>,
}

impl CleanupEngine {
    /// Engine over a shared registry.
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }

    /// Reclaim everything in `Expired` or `PendingCleanup` state.
    pub fn cleanup(&self) -> Result<CleanupReport, RegistryError> {
        let started = Instant::now();
        let mut report = CleanupReport::default();
        for meta in self.registry.snapshot()? {
            report.scanned += 1;
            if !meta.state.is_cleanup_eligible() {
                report.skipped += 1;
                continue;
            }
            match self.registry.remove(&meta.id) {
                Ok(removed) => {
                    report.removed += 1;
                    report.bytes_freed = report.bytes_freed.saturating_add(removed.size_bytes);
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
    use crate::core::resource::{ResourceKind, ResourceMeta};

    #[test]
    fn reclaims_only_eligible_resources() {
        let reg = Arc::new(Registry::new());
        let engine = CleanupEngine::new(reg.clone());

        let mut expired = ResourceMeta::new("gone", "ns", ResourceKind::File);
        expired.size_bytes = 42;
        expired.state = crate::core::lifecycle::ResourceState::Expired;
        let active = ResourceMeta::new("live", "ns", ResourceKind::File);
        reg.register(expired).unwrap();
        reg.register(active).unwrap();

        let report = engine.cleanup().unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.removed, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.bytes_freed, 42);
        assert_eq!(reg.len().unwrap(), 1);
    }
}
