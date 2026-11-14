//! Cleanup engine and its report type.

mod collector;
mod report;

/// Maximum number of cleanup attempts before a resource is left alone.
///
/// A resource whose physical removal fails lands in `FailedCleanup` and
/// is retried on later passes; after this many attempts the pass stops
/// touching it so transient failures cannot loop forever.
pub const MAX_CLEANUP_ATTEMPTS: u32 = 3;

pub use collector::CleanupEngine;
pub(crate) use collector::SweepView;
pub use report::{CleanupReport, RemovalReasons};
