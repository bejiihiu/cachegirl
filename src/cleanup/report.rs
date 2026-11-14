//! Aggregate result of a cleanup pass.

use std::fmt;
use std::time::Duration;

/// What a cleanup pass did, in numbers.
///
/// One failing resource never aborts the pass; it lands in `failed`
/// and the pass continues with the rest.
#[derive(Clone, Debug, Default)]
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
    pub deferred: u64,
    /// Total bytes reclaimed.
    pub bytes_freed: u64,
    /// Wall-clock time the pass took.
    pub duration: Duration,
    /// Why individual failures happened.
    pub failure_reasons: Vec<String>,
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
        )
    }
}
