//! Aggregate result of a cleanup pass.

use core::fmt;
use core::time::Duration;

/// Per-reason accounting of what (and why) a pass removed.
///
/// `lease_deferred` counts destruction attempts that were held back by
/// active leases; everything else counts resources actually unregistered
/// and destroyed. `eligible` covers resources that were already marked
/// for cleanup before the pass noticed them (or being retried after a
/// previous failure).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RemovalReasons {
    /// Expired through a fixed TTL counted from creation.
    pub ttl_fixed: u64,
    /// Expired through an idle TTL counted from the last access.
    pub ttl_idle: u64,
    /// Evicted to fit a byte quota.
    pub size_eviction: u64,
    /// Evicted to fit a resource count quota.
    pub count_eviction: u64,
    /// Marked for removal by a custom cleanup policy.
    pub policy: u64,
    /// Held back because leases were still active.
    pub lease_deferred: u64,
    /// Already flagged for cleanup before this pass (or retried after a failure).
    pub eligible: u64,
}

impl RemovalReasons {
    /// Total destructions attributed to a cause, excluding deferrals.
    pub fn destroyed(&self) -> u64 {
        self.ttl_fixed
            + self.ttl_idle
            + self.size_eviction
            + self.count_eviction
            + self.policy
            + self.eligible
    }

    /// True when nothing was removed and nothing was deferred.
    pub fn is_quiet(&self) -> bool {
        self.destroyed() == 0 && self.lease_deferred == 0
    }

    fn nonzero_entries(&self) -> Vec<(&'static str, u64)> {
        let mut out = Vec::new();
        for (name, value) in [
            ("ttl_fixed", self.ttl_fixed),
            ("ttl_idle", self.ttl_idle),
            ("size_eviction", self.size_eviction),
            ("count_eviction", self.count_eviction),
            ("policy", self.policy),
            ("lease_deferred", self.lease_deferred),
            ("eligible", self.eligible),
        ] {
            if value > 0 {
                out.push((name, value));
            }
        }
        out
    }
}

/// What a cleanup pass did, in numbers.
///
/// One failing resource never aborts the pass; it lands in `failed`
/// and the pass continues with the rest. `reasons` breaks `removed`
/// and `deferred` down by cause so callers can tell *why* the pass
/// did what it did.
///
/// `duration` serializes as `{"secs": ..., "nanos": ...}`. `deferred`,
/// `failure_reasons` and `reasons` default when absent so reports
/// written by older versions still load. Unknown fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CleanupReport {
    /// Resources examined.
    pub scanned: u64,
    /// Resources reclaimed.
    pub removed: u64,
    /// Resources skipped, e.g. still active or locked.
    pub skipped: u64,
    /// Resources that were tried but could not be reclaimed.
    pub failed: u64,
    /// Resources held back because leases were still active.
    #[cfg_attr(feature = "serde", serde(default))]
    pub deferred: u64,
    /// Total bytes reclaimed.
    pub bytes_freed: u64,
    /// Wall-clock time the pass took.
    pub duration: Duration,
    /// Why individual failures happened.
    #[cfg_attr(feature = "serde", serde(default))]
    pub failure_reasons: Vec<String>,
    /// Per-cause accounting of removals and deferrals.
    #[cfg_attr(feature = "serde", serde(default))]
    pub reasons: RemovalReasons,
}

impl CleanupReport {
    /// True when nothing was removed and nothing failed.
    pub fn is_quiet(&self) -> bool {
        self.removed == 0 && self.failed == 0
    }
}

impl fmt::Display for CleanupReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cleanup: scanned={} removed={} skipped={} failed={} deferred={} bytes_freed={} in {:?}",
            self.scanned,
            self.removed,
            self.skipped,
            self.failed,
            self.deferred,
            self.bytes_freed,
            self.duration
        )?;
        let reasons = self.reasons.nonzero_entries();
        if !reasons.is_empty() {
            let breakdown = reasons
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, " ({breakdown})")?;
        }
        Ok(())
    }
}
