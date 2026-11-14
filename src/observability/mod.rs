//! Observability: statistics snapshots and structured events.

mod events;
mod statistics;

#[cfg(feature = "tracing")]
pub use events::TracingSink;
pub use events::{Event, EventSink, NullSink, VecSink};
pub use statistics::{Statistics, StatisticsCollector};
