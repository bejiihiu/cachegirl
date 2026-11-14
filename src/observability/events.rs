//! Structured events emitted by manager operations.
//!
//! Events are plain data. Sinks decide where they go: a vec for tests,
//! `tracing` when the feature is enabled, or your own implementation.

use std::sync::Mutex;

use crate::core::resource::{ResourceId, ResourceKind};

/// Something notable that happened inside the manager.
///
/// Enums serialize externally tagged, e.g.
/// `{"ResourceCreated": {"instance": ..., "id": ..., ...}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Event {
    /// A resource was registered.
    ResourceCreated {
        /// Instance that created the resource.
        instance: String,
        /// Resource id.
        id: ResourceId,
        /// Resource kind.
        kind: ResourceKind,
        /// Namespace it was created in.
        namespace: String,
    },
    /// A resource was reclaimed.
    ResourceReleased {
        /// Instance that released the resource.
        instance: String,
        /// Resource id.
        id: ResourceId,
        /// Bytes reclaimed.
        bytes_freed: u64,
    },
    /// A cleanup pass finished.
    CleanupCompleted {
        /// Instance that ran the pass.
        instance: String,
        /// Resources examined.
        scanned: u64,
        /// Resources reclaimed.
        removed: u64,
        /// Failures.
        failed: u64,
        /// Bytes reclaimed.
        bytes_freed: u64,
    },
    /// A recovery pass finished after a crash.
    RecoveryCompleted {
        /// Instance that ran the pass.
        instance: String,
        /// Instance directories examined.
        scanned: u64,
        /// Stale directories removed.
        removed: u64,
        /// Directories kept.
        kept: u64,
        /// Directories that could not be reclaimed.
        failed: u64,
        /// Bytes freed by reclamation.
        bytes_freed: u64,
    },
}

/// Destination for [`Event`]s.
///
/// Sinks are called synchronously from the operations that emit them,
/// including from destructors. They should be cheap and they should not
/// panic: a panicking sink aborts the operation it was attached to and,
/// when destruction is in progress, leaves the resource's bookkeeping
/// in an unknown state (the physical side may already be gone).
pub trait EventSink: Send + Sync {
    /// Consume one event.
    fn emit(&self, event: &Event);
}

/// Records events in memory; useful for tests and diagnostics.
#[derive(Default)]
pub struct VecSink {
    events: Mutex<Vec<Event>>,
}

impl VecSink {
    /// Create an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain all recorded events.
    pub fn drain(&self) -> Vec<Event> {
        self.events
            .lock()
            .map(|mut v| core::mem::take(&mut *v))
            .unwrap_or_default()
    }

    /// How many events were recorded.
    pub fn len(&self) -> usize {
        self.events.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl EventSink for VecSink {
    fn emit(&self, event: &Event) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

/// Emits events as `tracing` debug records. Only with the `tracing` feature.
#[cfg(feature = "tracing")]
#[derive(Debug, Default)]
pub struct TracingSink;

#[cfg(feature = "tracing")]
impl TracingSink {
    /// Create the sink.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "tracing")]
impl EventSink for TracingSink {
    fn emit(&self, event: &Event) {
        match event {
            Event::ResourceCreated {
                instance,
                id,
                kind,
                namespace,
            } => {
                tracing::debug!(%instance, %id, %kind, %namespace, "resource created");
            }
            Event::ResourceReleased {
                instance,
                id,
                bytes_freed,
            } => {
                tracing::debug!(%instance, %id, bytes_freed, "resource released");
            }
            Event::CleanupCompleted {
                instance,
                scanned,
                removed,
                failed,
                bytes_freed,
            } => {
                tracing::debug!(%instance, scanned, removed, failed, bytes_freed, "cleanup completed");
            }
            Event::RecoveryCompleted {
                instance,
                scanned,
                removed,
                kept,
                failed,
                bytes_freed,
            } => {
                tracing::debug!(%instance, scanned, removed, kept, failed, bytes_freed, "recovery completed");
            }
        }
    }
}

/// Silence for applications that do not want events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &Event) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_sink_records_events() {
        let sink = VecSink::new();
        assert!(sink.is_empty());
        sink.emit(&Event::ResourceReleased {
            instance: "i".into(),
            id: ResourceId::new(),
            bytes_freed: 10,
        });
        assert_eq!(sink.len(), 1);
        assert_eq!(sink.drain().len(), 1);
        assert!(sink.is_empty());
    }

    #[test]
    fn drain_preserves_emission_order() {
        let sink = VecSink::new();
        let id = ResourceId::new();
        sink.emit(&Event::ResourceCreated {
            instance: "i".into(),
            id: id.clone(),
            kind: crate::core::resource::ResourceKind::File,
            namespace: "ns".into(),
        });
        sink.emit(&Event::ResourceReleased {
            instance: "i".into(),
            id: id.clone(),
            bytes_freed: 1,
        });
        sink.emit(&Event::CleanupCompleted {
            instance: "i".into(),
            scanned: 1,
            removed: 1,
            failed: 0,
            bytes_freed: 1,
        });
        let drained = sink.drain();
        assert_eq!(drained.len(), 3);
        assert!(matches!(drained[0], Event::ResourceCreated { .. }));
        assert!(matches!(drained[1], Event::ResourceReleased { .. }));
        assert!(matches!(drained[2], Event::CleanupCompleted { .. }));
    }

    #[test]
    fn drain_on_an_empty_sink_yields_nothing() {
        let sink = VecSink::new();
        assert!(sink.drain().is_empty());
        assert_eq!(sink.len(), 0);
    }

    #[test]
    fn emit_after_drain_starts_from_a_clean_slate() {
        let sink = VecSink::new();
        sink.emit(&Event::ResourceReleased {
            instance: "i".into(),
            id: ResourceId::new(),
            bytes_freed: 1,
        });
        let _ = sink.drain();
        sink.emit(&Event::ResourceReleased {
            instance: "i".into(),
            id: ResourceId::new(),
            bytes_freed: 2,
        });
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert!(matches!(
            drained[0],
            Event::ResourceReleased { bytes_freed: 2, .. }
        ));
    }

    #[test]
    fn null_sink_swallows_every_event_variant() {
        let sink = NullSink;
        let id = ResourceId::new();
        // none of these may panic; NullSink is the sink for apps that
        // do not want any observability output
        sink.emit(&Event::ResourceCreated {
            instance: "i".into(),
            id: id.clone(),
            kind: crate::core::resource::ResourceKind::File,
            namespace: "ns".into(),
        });
        sink.emit(&Event::ResourceReleased {
            instance: "i".into(),
            id: id.clone(),
            bytes_freed: 1,
        });
        sink.emit(&Event::CleanupCompleted {
            instance: "i".into(),
            scanned: 1,
            removed: 1,
            failed: 0,
            bytes_freed: 1,
        });
        sink.emit(&Event::RecoveryCompleted {
            instance: "i".into(),
            scanned: 1,
            removed: 0,
            kept: 1,
            failed: 0,
            bytes_freed: 0,
        });
    }

    #[test]
    fn events_compare_structurally() {
        let id = ResourceId::new();
        let make = || Event::ResourceReleased {
            instance: "i".into(),
            id: id.clone(),
            bytes_freed: 10,
        };
        assert_eq!(make(), make());
        assert_ne!(
            make(),
            Event::ResourceReleased {
                instance: "i".into(),
                id,
                bytes_freed: 11,
            }
        );
    }
}
