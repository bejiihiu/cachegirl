//! Observability: statistics snapshots, structured events and optional
//! `tracing` / `metrics` instrumentation.
//!
//! The `metrics` feature instruments operations through the `metrics`
//! facade (the crate never picks an exporter); the `tracing` feature does
//! the same through `tracing` spans and events. Both compile to nothing
//! when disabled.

mod events;
pub(crate) mod metrics_support;
mod statistics;

#[cfg(feature = "tracing")]
pub use events::TracingSink;
pub use events::{Event, EventSink, NullSink, VecSink};
pub use statistics::{Statistics, StatisticsCollector};
