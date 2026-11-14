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
    ///
    /// Saturates at `u64::MAX` instead of overflowing on fabricated
    /// out-of-range counts.
    pub fn destroyed(&self) -> u64 {
        self.ttl_fixed
            .saturating_add(self.ttl_idle)
            .saturating_add(self.size_eviction)
            .saturating_add(self.count_eviction)
            .saturating_add(self.policy)
            .saturating_add(self.eligible)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destroyed_sums_every_removal_cause_but_not_deferrals() {
        let reasons = RemovalReasons {
            ttl_fixed: 1,
            ttl_idle: 2,
            size_eviction: 4,
            count_eviction: 8,
            policy: 16,
            lease_deferred: 32,
            eligible: 64,
        };
        assert_eq!(reasons.destroyed(), 95);
    }

    #[test]
    fn destroyed_saturates_instead_of_overflowing() {
        let reasons = RemovalReasons {
            ttl_fixed: u64::MAX,
            ttl_idle: u64::MAX,
            policy: u64::MAX,
            ..RemovalReasons::default()
        };
        assert_eq!(reasons.destroyed(), u64::MAX);
    }

    #[test]
    fn is_quiet_requires_nothing_removed_and_nothing_deferred() {
        assert!(RemovalReasons::default().is_quiet());
        assert!(
            !RemovalReasons {
                lease_deferred: 1,
                ..RemovalReasons::default()
            }
            .is_quiet()
        );
        assert!(
            !RemovalReasons {
                ttl_fixed: 1,
                ..RemovalReasons::default()
            }
            .is_quiet()
        );
    }

    #[test]
    fn report_is_quiet_ignores_skips_and_deferrals() {
        // "quiet" means nothing was reclaimed and nothing failed; a pass
        // that only skipped or deferred counts as quiet
        let report = CleanupReport {
            skipped: 5,
            deferred: 3,
            ..CleanupReport::default()
        };
        assert!(report.is_quiet());
        assert!(
            !CleanupReport {
                removed: 1,
                ..CleanupReport::default()
            }
            .is_quiet()
        );
        assert!(
            !CleanupReport {
                failed: 1,
                ..CleanupReport::default()
            }
            .is_quiet()
        );
    }

    #[test]
    fn display_shows_only_nonzero_reasons() {
        let report = CleanupReport {
            scanned: 10,
            removed: 3,
            skipped: 7,
            failed: 0,
            deferred: 0,
            bytes_freed: 128,
            duration: Duration::from_millis(3),
            failure_reasons: Vec::new(),
            reasons: RemovalReasons {
                ttl_fixed: 2,
                policy: 1,
                ..RemovalReasons::default()
            },
        };
        let rendered = report.to_string();
        assert!(rendered.contains("scanned=10"));
        assert!(rendered.contains("removed=3"));
        assert!(rendered.contains("bytes_freed=128"));
        assert!(rendered.contains("ttl_fixed=2"));
        assert!(rendered.contains("policy=1"));
        assert!(!rendered.contains("ttl_idle"));
        assert!(!rendered.contains("eligible"));
    }

    #[test]
    fn display_without_reasons_has_no_breakdown_section() {
        let report = CleanupReport::default();
        let rendered = report.to_string();
        assert!(!rendered.contains('('));
        assert!(rendered.contains("scanned=0"));
    }

    #[test]
    fn display_lists_every_nonzero_reason_in_fixed_order() {
        let report = CleanupReport {
            reasons: RemovalReasons {
                ttl_fixed: 1,
                ttl_idle: 2,
                size_eviction: 3,
                count_eviction: 4,
                policy: 5,
                lease_deferred: 6,
                eligible: 7,
            },
            ..CleanupReport::default()
        };
        let rendered = report.to_string();
        let order = rendered
            .split('(')
            .nth(1)
            .unwrap_or_default()
            .split(',')
            .map(|entry| {
                entry
                    .trim()
                    .split('=')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>();
        // fixed display order keeps logs greppable regardless of which
        // reasons actually fired
        assert_eq!(
            order,
            vec![
                "ttl_fixed",
                "ttl_idle",
                "size_eviction",
                "count_eviction",
                "policy",
                "lease_deferred",
                "eligible"
            ]
        );
    }
}
