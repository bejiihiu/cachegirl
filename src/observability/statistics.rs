//! Point-in-time statistics about manager state.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::core::registry::Registry;

/// Plain serializable snapshot of manager statistics.
///
/// `captured_at` serializes as `{"secs_since_epoch": ..., "nanos_since_epoch": ...}`
/// and may be `null`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Statistics {
    /// Instance the snapshot describes.
    pub instance_id: String,
    /// When the snapshot was taken.
    pub captured_at: Option<SystemTime>,
    /// Live namespaces.
    pub namespaces: u64,
    /// Live registered resources.
    pub resources: u64,
    /// Bytes tracked across all resources.
    pub bytes_tracked: u64,
    /// Resources currently expired but not yet cleaned.
    pub expired: u64,
    /// Resources queued for cleanup.
    pub pending_cleanup: u64,
    /// Cleanup passes completed.
    pub gc_runs: u64,
    /// Resources released since startup.
    pub resources_released: u64,
    /// Bytes freed since startup.
    pub bytes_freed: u64,
    /// Leases currently held across all resources.
    pub active_leases: u64,
}

impl Statistics {
    /// Human-readable multi-line summary, handy for logs and diagnostics.
    pub fn render(&self) -> String {
        format!(
            "cachegirl statistics\n  instance:  {}\n  namespaces: {}\n  resources:  {}\n  tracked:    {} bytes\n  expired:    {}\n  pending:    {}\n  leases:     {}\n  gc runs:    {}\n  released:   {}\n  freed:      {} bytes",
            self.instance_id,
            self.namespaces,
            self.resources,
            self.bytes_tracked,
            self.expired,
            self.pending_cleanup,
            self.active_leases,
            self.gc_runs,
            self.resources_released,
            self.bytes_freed,
        )
    }
}

/// Atomic counters feeding [`Statistics`]; the registry supplies the rest.
#[derive(Default)]
pub struct StatisticsCollector {
    gc_runs: AtomicU64,
    resources_released: AtomicU64,
    bytes_freed: AtomicU64,
    active_leases: AtomicU64,
}

impl StatisticsCollector {
    /// Create a zeroed collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one completed cleanup pass.
    ///
    /// Only bumps the pass counter: the resources reclaimed during the
    /// pass are accounted for by the destruction path itself.
    pub fn record_gc_run(&self) {
        self.gc_runs.fetch_add(1, Ordering::Relaxed);
    }

    /// Record resources released outside cleanup passes (e.g. explicit release).
    pub fn record_release(&self, removed: u64, bytes_freed: u64) {
        self.resources_released
            .fetch_add(removed, Ordering::Relaxed);
        self.bytes_freed.fetch_add(bytes_freed, Ordering::Relaxed);
    }

    /// Record one lease being acquired.
    pub(crate) fn record_lease_acquire(&self) {
        self.active_leases.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one lease being released.
    pub(crate) fn record_lease_release(&self) {
        // saturating decrement, spelled out so it works on the MSRV
        // and never wraps to a huge number on a stray release
        let mut current = self.active_leases.load(Ordering::Relaxed);
        while current > 0 {
            match self.active_leases.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Build a snapshot from the collector and a registry.
    pub fn snapshot(
        &self,
        instance_id: &str,
        namespaces: u64,
        registry: &Registry,
    ) -> crate::core::error::Result<Statistics> {
        let metas = registry.snapshot()?;
        let expired = metas
            .iter()
            .filter(|m| m.state == crate::core::lifecycle::ResourceState::Expired)
            .count() as u64;
        let pending = metas
            .iter()
            .filter(|m| m.state == crate::core::lifecycle::ResourceState::PendingCleanup)
            .count() as u64;
        let bytes = crate::core::saturating_bytes(metas.iter().map(|m| m.size_bytes));
        Ok(Statistics {
            instance_id: instance_id.to_owned(),
            captured_at: Some(SystemTime::now()),
            namespaces,
            resources: metas.len() as u64,
            bytes_tracked: bytes,
            expired,
            pending_cleanup: pending,
            gc_runs: self.gc_runs.load(Ordering::Relaxed),
            resources_released: self.resources_released.load(Ordering::Relaxed),
            bytes_freed: self.bytes_freed.load(Ordering::Relaxed),
            active_leases: self.active_leases.load(Ordering::Relaxed),
        })
    }

    /// Dump the counters held by this collector as gauges via the
    /// `metrics` facade, when the `metrics` feature is on.
    ///
    /// This is a point-in-time snapshot for debugging; the operations
    /// themselves maintain the live counters and gauges
    /// (`cachegirl_cleanup_runs_total`, `cachegirl_active_resources`,
    /// `cachegirl_active_leases`, ...) as they go.
    #[cfg(feature = "metrics")]
    pub fn emit_metrics(&self, instance_id: &str) {
        metrics::gauge!("cachegirl_gc_runs_total", "instance" => instance_id.to_owned())
            .set(self.gc_runs.load(Ordering::Relaxed) as f64);
        metrics::gauge!("cachegirl_resources_released_total", "instance" => instance_id.to_owned())
            .set(self.resources_released.load(Ordering::Relaxed) as f64);
        metrics::gauge!("cachegirl_bytes_freed_total", "instance" => instance_id.to_owned())
            .set(self.bytes_freed.load(Ordering::Relaxed) as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::{ResourceKind, ResourceMeta};

    #[test]
    fn snapshot_reflects_registry() {
        let reg = Registry::new();
        let mut meta = ResourceMeta::new("x", "ns", ResourceKind::File);
        meta.size_bytes = 100;
        reg.register(meta).unwrap();
        let collector = StatisticsCollector::new();
        collector.record_gc_run();
        collector.record_release(3, 300);
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.resources, 1);
        assert_eq!(snap.bytes_tracked, 100);
        assert_eq!(snap.gc_runs, 1);
        assert_eq!(snap.resources_released, 3);
        assert_eq!(snap.bytes_freed, 300);
        assert_eq!(snap.active_leases, 0);
    }

    #[test]
    fn active_leases_counter_moves() {
        let collector = StatisticsCollector::new();
        collector.record_lease_acquire();
        collector.record_lease_acquire();
        let reg = Registry::new();
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.active_leases, 2);
        collector.record_lease_release();
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.active_leases, 1);
        // a stray release must not wrap around
        collector.record_lease_release();
        collector.record_lease_release();
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.active_leases, 0);
    }

    #[test]
    fn snapshot_counts_expired_and_pending_cleanup_separately() {
        let reg = Registry::new();
        let mut active = ResourceMeta::new("live", "ns", ResourceKind::File);
        active.state = crate::core::lifecycle::ResourceState::Active;
        let mut expired = ResourceMeta::new("old", "ns", ResourceKind::File);
        expired.state = crate::core::lifecycle::ResourceState::Expired;
        let mut pending = ResourceMeta::new("queued", "ns", ResourceKind::File);
        pending.state = crate::core::lifecycle::ResourceState::PendingCleanup;
        for m in [active, expired, pending] {
            reg.register(m).unwrap();
        }
        let collector = StatisticsCollector::new();
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.resources, 3);
        assert_eq!(snap.expired, 1);
        assert_eq!(snap.pending_cleanup, 1);
    }

    #[test]
    fn gc_runs_and_plain_releases_accumulate_independently() {
        let reg = Registry::new();
        let collector = StatisticsCollector::new();
        collector.record_gc_run();
        collector.record_gc_run();
        collector.record_release(6, 350);
        let snap = collector.snapshot("instance-test", 1, &reg).unwrap();
        assert_eq!(snap.gc_runs, 2);
        assert_eq!(snap.resources_released, 6);
        assert_eq!(snap.bytes_freed, 350);
    }

    #[test]
    fn empty_registry_yields_a_zeroed_snapshot() {
        let reg = Registry::new();
        let collector = StatisticsCollector::new();
        let before = SystemTime::now();
        let snap = collector.snapshot("instance-test", 0, &reg).unwrap();
        assert_eq!(snap.resources, 0);
        assert_eq!(snap.bytes_tracked, 0);
        assert_eq!(snap.expired, 0);
        assert_eq!(snap.pending_cleanup, 0);
        assert_eq!(snap.gc_runs, 0);
        let taken = snap.captured_at.expect("snapshot must record its time");
        assert!(taken >= before);
    }

    #[test]
    fn render_mentions_instance_and_counters() {
        let snap = Statistics {
            instance_id: "instance-xyz".into(),
            ..Statistics::default()
        };
        let rendered = snap.render();
        assert!(rendered.contains("instance-xyz"));
        assert!(rendered.contains("resources:"));
        assert!(rendered.contains("leases:"));
        assert!(rendered.contains("gc runs:"));
    }
}
