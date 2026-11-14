//! End-to-end checks that the `tracing` and `metrics` integrations are
//! wired into the real operation paths, not just compiling.
//!
//! Everything here uses purpose-built capture subscribers/recorders:
//! tracing goes through a `Layer` on the registry scoped with
//! `with_default` (thread-local, isolated per test); metrics go through
//! the facade's `with_local_recorder` with a recorder that simply stores
//! values in maps. Neither is a mock вЂ” both are real implementations of
//! the respective extension points.

#![cfg(all(feature = "filesystem", any(feature = "tracing", feature = "metrics")))]

fn unique_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cachegirl-obs-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

#[cfg(feature = "metrics")]
mod metrics_tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Label, Metadata, Recorder, SharedString, Unit,
    };

    // -- a capturing recorder: the real `Recorder` contract, values in maps --

    struct CaptureCounter(Arc<AtomicU64>);
    impl metrics::CounterFn for CaptureCounter {
        fn increment(&self, value: u64) {
            self.0.fetch_add(value, Ordering::Relaxed);
        }
        fn absolute(&self, value: u64) {
            self.0.store(value, Ordering::Relaxed);
        }
    }

    struct CaptureGauge(Arc<AtomicU64>); // f64 bits
    impl metrics::GaugeFn for CaptureGauge {
        fn increment(&self, value: f64) {
            fetch_add_f64(&self.0, value);
        }
        fn decrement(&self, value: f64) {
            fetch_add_f64(&self.0, -value);
        }
        fn set(&self, value: f64) {
            self.0.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    fn fetch_add_f64(cell: &AtomicU64, delta: f64) {
        let mut current = cell.load(Ordering::Relaxed);
        loop {
            let next = (f64::from_bits(current) + delta).to_bits();
            match cell.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    struct CaptureHistogram(Arc<Mutex<Vec<f64>>>);
    impl metrics::HistogramFn for CaptureHistogram {
        fn record(&self, value: f64) {
            self.0.lock().unwrap().push(value);
        }
    }

    enum Captured {
        Counter(Arc<AtomicU64>),
        Gauge(Arc<AtomicU64>),
        Histogram(Arc<Mutex<Vec<f64>>>),
    }

    /// Labels are normalized by sorting, so tests don't depend on the
    /// order the emission sites wrote them in.
    fn normalized(key: &Key) -> Key {
        let mut labels: Vec<Label> = key.labels().cloned().collect();
        labels.sort_by(|a, b| a.key().cmp(b.key()).then(a.value().cmp(b.value())));
        Key::from_parts(key.name().to_owned(), labels)
    }

    fn expected_key(name: &str, labels: &[(&str, &str)]) -> Key {
        let mut labels: Vec<Label> = labels
            .iter()
            .map(|(k, v)| Label::new(k.to_string(), v.to_string()))
            .collect();
        labels.sort_by(|a, b| a.key().cmp(b.key()).then(a.value().cmp(b.value())));
        Key::from_parts(name.to_owned(), labels)
    }

    #[derive(Default)]
    struct CaptureRecorder {
        captured: Mutex<HashMap<Key, Captured>>,
    }

    impl CaptureRecorder {
        fn with_metrics<R>(&self, f: impl FnOnce() -> R) -> R {
            metrics::with_local_recorder(self, f)
        }

        fn cell(&self, key: &Key) -> Option<Captured> {
            let map = self.captured.lock().unwrap();
            match map.get(key) {
                Some(Captured::Counter(cell)) => Some(Captured::Counter(Arc::clone(cell))),
                Some(Captured::Gauge(cell)) => Some(Captured::Gauge(Arc::clone(cell))),
                Some(Captured::Histogram(cell)) => Some(Captured::Histogram(Arc::clone(cell))),
                None => None,
            }
        }

        fn counter_value(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            let key = expected_key(name, labels);
            match self.cell(&key) {
                Some(Captured::Counter(cell)) => cell.load(Ordering::Relaxed),
                _ => 0,
            }
        }

        fn gauge_value(&self, name: &str) -> f64 {
            let key = expected_key(name, &[]);
            match self.cell(&key) {
                Some(Captured::Gauge(cell)) => f64::from_bits(cell.load(Ordering::Relaxed)),
                _ => 0.0,
            }
        }

        fn histogram_samples(&self, name: &str, labels: &[(&str, &str)]) -> Vec<f64> {
            let key = expected_key(name, labels);
            match self.cell(&key) {
                Some(Captured::Histogram(cell)) => cell.lock().unwrap().clone(),
                _ => Vec::new(),
            }
        }
    }

    impl Recorder for CaptureRecorder {
        fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {
        }
        fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}
        fn describe_histogram(
            &self,
            _key: KeyName,
            _unit: Option<Unit>,
            _description: SharedString,
        ) {
        }

        fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
            let mut map = self.captured.lock().unwrap();
            let normalized = normalized(key);
            let cell = match map
                .entry(normalized)
                .or_insert_with(|| Captured::Counter(Arc::new(AtomicU64::new(0))))
            {
                Captured::Counter(cell) => Arc::clone(cell),
                _ => panic!("metric name reused with a different kind"),
            };
            Counter::from_arc(Arc::new(CaptureCounter(cell)))
        }

        fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> Gauge {
            let mut map = self.captured.lock().unwrap();
            let normalized = normalized(key);
            let cell = match map
                .entry(normalized)
                .or_insert_with(|| Captured::Gauge(Arc::new(AtomicU64::new(0.0f64.to_bits()))))
            {
                Captured::Gauge(cell) => Arc::clone(cell),
                _ => panic!("metric name reused with a different kind"),
            };
            Gauge::from_arc(Arc::new(CaptureGauge(cell)))
        }

        fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> Histogram {
            let mut map = self.captured.lock().unwrap();
            let normalized = normalized(key);
            let cell = match map
                .entry(normalized)
                .or_insert_with(|| Captured::Histogram(Arc::new(Mutex::new(Vec::new()))))
            {
                Captured::Histogram(cell) => Arc::clone(cell),
                _ => panic!("metric name reused with a different kind"),
            };
            Histogram::from_arc(Arc::new(CaptureHistogram(cell)))
        }
    }

    #[test]
    fn created_released_and_bytes_are_counted() {
        let recorder = CaptureRecorder::default();
        let dir = unique_dir("counters");
        recorder.with_metrics(|| {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-metrics-counters")
                .build()
                .unwrap();
            let ns = manager.namespace("metrics-counters").unwrap();
            let file = ns.create_file("payload.bin").unwrap();
            std::fs::write(file.path(), vec![7u8; 512]).unwrap();
            let id = file.id().clone();
            assert_eq!(ns.refresh_size(&id).unwrap(), 512);
            ns.release(&id).unwrap();
            manager.shutdown().unwrap();
        });
        let created = recorder.counter_value(
            "cachegirl_resources_created_total",
            &[
                ("namespace", "metrics-counters"),
                ("kind", "file"),
                ("result", "ok"),
            ],
        );
        assert_eq!(created, 1);
        let released = recorder.counter_value(
            "cachegirl_resources_released_total",
            &[
                ("namespace", "metrics-counters"),
                ("kind", "file"),
                ("result", "ok"),
            ],
        );
        assert_eq!(released, 1);
        assert_eq!(
            recorder.counter_value("cachegirl_bytes_freed_total", &[]),
            512
        );
        assert_eq!(recorder.gauge_value("cachegirl_active_resources"), 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn created_counter_moves_with_kind_and_result() {
        let recorder = CaptureRecorder::default();
        let dir = unique_dir("kinds");
        recorder.with_metrics(|| {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-metrics-kinds")
                .build()
                .unwrap();
            let ns = manager.namespace("metrics-kinds").unwrap();
            let _dir_res = ns.create_directory("work").unwrap();
            manager.shutdown().unwrap();
        });
        let created = recorder.counter_value(
            "cachegirl_resources_created_total",
            &[
                ("namespace", "metrics-kinds"),
                ("kind", "directory"),
                ("result", "ok"),
            ],
        );
        assert_eq!(created, 1);
        assert_eq!(recorder.gauge_value("cachegirl_active_resources"), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quota_rejections_are_counted_with_reason() {
        let recorder = CaptureRecorder::default();
        let dir = unique_dir("quota");
        recorder.with_metrics(|| {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-metrics-quota")
                .build()
                .unwrap();
            let ns = manager.namespace("metrics-quota").unwrap();
            ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(1));
            let _first = ns.create_file("one.bin").unwrap();
            let rejected = ns.create_file("two.bin");
            assert!(matches!(
                rejected,
                Err(cachegirl::Error::Quota(
                    cachegirl::QuotaError::CountExceeded { .. }
                ))
            ));
            manager.shutdown().unwrap();
        });
        let rejected = recorder.counter_value(
            "cachegirl_quota_rejections_total",
            &[("namespace", "metrics-quota"), ("reason", "count")],
        );
        assert_eq!(rejected, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lease_acquire_release_and_deferred_ops_are_counted() {
        let recorder = CaptureRecorder::default();
        let dir = unique_dir("leases");
        recorder.with_metrics(|| {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-metrics-leases")
                .build()
                .unwrap();
            let ns = manager.namespace("metrics-leases").unwrap();
            let file = ns.create_file("leased.bin").unwrap();
            let id = file.id().clone();

            let lease = file.acquire().unwrap();
            assert_eq!(recorder.gauge_value("cachegirl_active_leases"), 1.0);
            // release defers while the lease is held; the counter must notice
            assert!(!ns.release(&id).unwrap());
            assert_eq!(
                recorder.counter_value(
                    "cachegirl_deferred_lease_operations_total",
                    &[("operation", "release")],
                ),
                1
            );
            drop(lease);
            assert_eq!(recorder.gauge_value("cachegirl_active_leases"), 0.0);
            manager.shutdown().unwrap();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_pass_counts_and_records_duration() {
        let recorder = CaptureRecorder::default();
        let dir = unique_dir("cleanup");
        recorder.with_metrics(|| {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-metrics-cleanup")
                .build()
                .unwrap();
            let ns = manager.namespace("metrics-cleanup").unwrap();
            use cachegirl::CreateOptions;
            let options = CreateOptions::new().ttl(std::time::Duration::ZERO);
            let expired = ns.create_file_with("overdue.bin", options).unwrap();
            let id = expired.id().clone();
            drop(expired);
            // a fixed zero TTL means the resource is already due; the pass reclaims it
            let report = manager.cleanup_expired().unwrap();
            assert_eq!(report.removed, 1, "{report}");
            let _ = id;
            manager.shutdown().unwrap();
        });
        assert_eq!(
            recorder.counter_value("cachegirl_cleanup_runs_total", &[("scope", "manager")]),
            1
        );
        let samples = recorder.histogram_samples(
            "cachegirl_cleanup_duration_seconds",
            &[("scope", "manager")],
        );
        assert_eq!(samples.len(), 1);
        assert!(samples[0] >= 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(feature = "tracing")]
mod tracing_tests {
    use super::*;

    use std::fmt;
    use std::sync::{Arc, Mutex};

    use tracing::Event;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        message: String,
        fields: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
        spans: Arc<Mutex<Vec<String>>>,
    }

    impl CaptureLayer {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                spans: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn take(&self) -> (Vec<CapturedEvent>, Vec<String>) {
            let events = std::mem::take(&mut *self.events.lock().unwrap());
            let spans = std::mem::take(&mut *self.spans.lock().unwrap());
            (events, spans)
        }
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.spans
                .lock()
                .unwrap()
                .push(attrs.metadata().name().to_string());
        }

        fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            struct Visitor {
                message: Option<String>,
                fields: Vec<String>,
            }
            impl Visit for Visitor {
                fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
                    let rendered = format!("{value:?}");
                    if field.name() == "message" {
                        self.message = Some(rendered);
                    } else {
                        self.fields.push(format!("{}={rendered}", field.name()));
                    }
                }
                fn record_str(&mut self, field: &Field, value: &str) {
                    if field.name() == "message" {
                        self.message = Some(value.to_string());
                    } else {
                        self.fields.push(format!("{}={value}", field.name()));
                    }
                }
            }
            let mut visitor = Visitor {
                message: None,
                fields: Vec::new(),
            };
            event.record(&mut visitor);
            self.events.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                message: visitor.message.unwrap_or_default(),
                fields: visitor.fields,
            });
        }
    }

    #[test]
    fn creation_release_and_cleanup_are_traced() {
        let capture = CaptureLayer::new();
        let subscriber = tracing_subscriber::registry().with(capture.clone());

        let dir = unique_dir("traced");
        tracing::subscriber::with_default(subscriber, || {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-tracing-paths")
                .build()
                .unwrap();
            let ns = manager.namespace("tracing-paths").unwrap();
            let file = ns.create_file("traced.bin").unwrap();
            let id = file.id().clone();
            let lease = file.acquire().unwrap();
            drop(lease);
            ns.release(&id).unwrap();
            manager.cleanup_expired().unwrap();
            manager.shutdown().unwrap();
        });

        let (events, spans) = capture.take();
        let messages: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("resource created")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("lease acquired")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("lease released")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("resource released")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("cleanup pass completed")),
            "{messages:?}"
        );
        assert!(spans.iter().any(|s| s == "create_resource"), "{spans:?}");
        assert!(spans.iter().any(|s| s == "cleanup_pass"), "{spans:?}");
        assert!(spans.iter().any(|s| s == "shutdown"), "{spans:?}");
        assert!(spans.iter().any(|s| s == "destroy_resource"), "{spans:?}");

        // structured fields survive the round trip
        let created = events
            .iter()
            .find(|e| e.message.contains("resource created"))
            .expect("create event");
        assert!(
            created.target.starts_with("cachegirl"),
            "{:?}",
            created.target
        );
        assert!(
            created.fields.iter().any(|f| f.starts_with("namespace=")),
            "{:?}",
            created.fields
        );
        assert!(
            created.fields.iter().any(|f| f.starts_with("kind=")),
            "{:?}",
            created.fields
        );

        let cleanup = events
            .iter()
            .find(|e| e.message.contains("cleanup pass completed"))
            .expect("cleanup event");
        assert_eq!(cleanup.level, tracing::Level::INFO);
        assert!(
            cleanup.fields.iter().any(|f| f.starts_with("scanned=")),
            "{:?}",
            cleanup.fields
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quota_rejection_is_logged_as_warning() {
        let capture = CaptureLayer::new();
        let subscriber = tracing_subscriber::registry().with(capture.clone());

        let dir = unique_dir("traced-quota");
        tracing::subscriber::with_default(subscriber, || {
            let manager = cachegirl::TempManager::builder()
                .root(&dir)
                .instance_id("instance-tracing-quota")
                .build()
                .unwrap();
            let ns = manager.namespace("tracing-quota").unwrap();
            ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(1));
            let _first = ns.create_file("one.bin").unwrap();
            let rejected = ns.create_file("two.bin");
            assert!(rejected.is_err());
            manager.shutdown().unwrap();
        });

        let (events, _spans) = capture.take();
        let warning = events
            .iter()
            .find(|e| e.message.contains("quota rejected"))
            .expect("a quota warning must be emitted");
        assert_eq!(warning.level, tracing::Level::WARN);
        assert!(
            warning.fields.iter().any(|f| f.contains("tracing-quota")),
            "{:?}",
            warning.fields
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
