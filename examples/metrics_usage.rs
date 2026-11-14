//! Collect cachegirl metrics into a small in-memory recorder and print
//! a summary at the end.
//!
//! The crate only emits through the `metrics` facade and never installs
//! an exporter by itself — in a real application you install your own
//! recorder (prometheus, statsd, a channel into your metrics pipeline,
//! ...) before building the manager. This example ships a minimal but
//! fully working `Recorder` that stores counters in a shared map, which
//! is also the pattern the integration tests use to verify emission.
//!
//! ```sh
//! cargo run --features metrics --example metrics_usage
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cachegirl::TempManager;
use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};

// -- a minimal but real recorder --------------------------------------------

struct CounterCell(AtomicU64);
impl metrics::CounterFn for CounterCell {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }
    fn absolute(&self, value: u64) {
        self.0.store(value, Ordering::Relaxed);
    }
}

struct GaugeCell(AtomicU64); // f64 bits
impl metrics::GaugeFn for GaugeCell {
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

struct HistogramCell(Mutex<Vec<f64>>);
impl metrics::HistogramFn for HistogramCell {
    fn record(&self, value: f64) {
        self.0.lock().unwrap().push(value);
    }
}

enum Captured {
    Counter(Arc<CounterCell>),
    Gauge(Arc<GaugeCell>),
    Histogram(Arc<HistogramCell>),
}

/// Stores every metric the application emits; read it back with `drain`.
#[derive(Default)]
struct MemoryRecorder {
    captured: Mutex<HashMap<Key, Captured>>,
}

impl MemoryRecorder {
    fn render(&self) -> String {
        let map = self.captured.lock().unwrap();
        let mut rows: Vec<(String, String)> = map
            .iter()
            .map(|(key, captured)| {
                let labels = key
                    .labels()
                    .map(|label| format!("{}={}", label.key(), label.value()))
                    .collect::<Vec<_>>()
                    .join(",");
                let value = match captured {
                    Captured::Counter(cell) => {
                        format!("counter = {}", cell.0.load(Ordering::Relaxed))
                    }
                    Captured::Gauge(cell) => {
                        format!("gauge = {}", f64::from_bits(cell.0.load(Ordering::Relaxed)))
                    }
                    Captured::Histogram(cell) => {
                        let samples = cell.0.lock().unwrap();
                        format!("histogram with {} sample(s)", samples.len())
                    }
                };
                (key.name().to_string(), format!("{labels} => {value}"))
            })
            .collect();
        rows.sort();
        let mut out = String::new();
        for (name, row) in rows {
            let _ = writeln!(out, "  {name:<40} {row}");
        }
        out
    }
}

impl Recorder for MemoryRecorder {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}
    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}
    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        let mut map = self.captured.lock().unwrap();
        let cell = match map
            .entry(key.clone())
            .or_insert_with(|| Captured::Counter(Arc::new(CounterCell(AtomicU64::new(0)))))
        {
            Captured::Counter(cell) => Arc::clone(cell),
            _ => panic!("metric name reused with a different kind"),
        };
        Counter::from_arc(Arc::clone(&cell))
    }

    fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        let mut map = self.captured.lock().unwrap();
        let cell = match map.entry(key.clone()).or_insert_with(|| {
            Captured::Gauge(Arc::new(GaugeCell(AtomicU64::new(0.0f64.to_bits()))))
        }) {
            Captured::Gauge(cell) => Arc::clone(cell),
            _ => panic!("metric name reused with a different kind"),
        };
        Gauge::from_arc(Arc::clone(&cell))
    }

    fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        let mut map = self.captured.lock().unwrap();
        let cell = match map
            .entry(key.clone())
            .or_insert_with(|| Captured::Histogram(Arc::new(HistogramCell(Mutex::new(Vec::new())))))
        {
            Captured::Histogram(cell) => Arc::clone(cell),
            _ => panic!("metric name reused with a different kind"),
        };
        Histogram::from_arc(Arc::clone(&cell))
    }
}

// -- the application ---------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let recorder = Arc::new(MemoryRecorder::default());
    metrics::set_global_recorder(Arc::clone(&recorder))
        .expect("the recorder may only be installed once");

    let dir = std::env::temp_dir().join(format!("cachegirl-metrics-usage-{}", std::process::id()));
    let manager = TempManager::builder()
        .root(&dir)
        .instance_id("instance-metrics-usage")
        .cleanup_on_shutdown(true)
        .build()?;

    let namespace = manager.namespace("demo")?;
    let file = namespace.create_file("sample.bin")?;
    let id = file.id().clone();

    // put some bytes in so the bytes-freed counter has something to say
    std::fs::write(file.path(), vec![42u8; 1024])?;
    namespace.refresh_size(&id)?;

    // a lease so the active-leases gauge moves
    let lease = file.acquire()?;
    drop(lease);
    namespace.release(&id)?;

    // one managed resource left behind on purpose: the active-resources
    // gauge keeps the count honest
    let _kept = namespace.create_file("kept.bin")?;

    manager.shutdown()?;

    println!("collected metrics:");
    print!("{}", recorder.render());

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
