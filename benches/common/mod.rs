//! Std-only harness for the cachegirl bench suite.
//!
//! Why this exists instead of a crates.io benchmark runner: this target
//! (windows-gnu) has no dlltool, so anything that drags in `windows-sys`
//! fails to link. Rather than skip the numbers, the suite times rounds
//! with `std::time::Instant` and reports percentiles over per-round
//! samples — same discipline as a statistical runner, zero dependencies.
//!
//! Sample ordering: every reported percentile is computed on the sorted
//! set of per-round wall times, nearest-rank. ops/sec comes from the
//! aggregate work, not from any single sample.

#![allow(dead_code)] // each bench target uses a subset

use std::hint::black_box;
use std::time::{Duration, Instant};

/// Knobs for one measurement series.
pub struct BenchConfig {
    /// Grep-friendly name, e.g. `allocation/buffer_create_release`.
    pub name: &'static str,
    /// Operations inside one round; divides round times into us/op.
    pub ops_per_round: usize,
    /// Warmup rounds, dropped from the samples.
    pub warmup_rounds: usize,
    /// Measured rounds.
    pub rounds: usize,
}

impl BenchConfig {
    pub const fn new(
        name: &'static str,
        ops_per_round: usize,
        warmup_rounds: usize,
        rounds: usize,
    ) -> Self {
        Self {
            name,
            ops_per_round,
            warmup_rounds,
            rounds,
        }
    }
}

/// Run `round` `warmup_rounds + rounds` times, print a one-line report.
///
/// The whole body of `round` is timed, so anything the round needs
/// pre-built belongs behind [`bench_setup`] instead.
pub fn bench(cfg: &BenchConfig, mut round: impl FnMut()) {
    bench_setup(cfg, || (), |_| round());
}

/// Like [`bench`], but each round gets a fresh `S` from `setup` first.
///
/// `setup` runs outside the timed window; use it for the one-off cost of
/// getting a round into a measurable state (e.g. expired resources for a
/// cleanup pass).
pub fn bench_setup<S>(
    cfg: &BenchConfig,
    mut setup: impl FnMut() -> S,
    mut round: impl FnMut(&mut S),
) {
    for _ in 0..cfg.warmup_rounds {
        let mut state = setup();
        round(&mut state);
    }

    let mut samples = Vec::with_capacity(cfg.rounds);
    for _ in 0..cfg.rounds {
        let mut state = setup();
        let start = Instant::now();
        round(&mut state);
        samples.push(start.elapsed());
    }

    report(cfg, &mut samples);
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    // nearest-rank on the sorted sample set
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    sorted[rank.max(1).min(n) - 1]
}

fn micros(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000_000.0
}

fn report(cfg: &BenchConfig, samples: &mut [Duration]) {
    samples.sort_unstable();
    let n = samples.len();
    let total: Duration = samples.iter().sum();
    let mean = total / n as u32;
    let ops = (cfg.ops_per_round * n) as f64;
    let ops_per_sec = ops / total.as_secs_f64();
    let op_us = |d: Duration| micros(d) / cfg.ops_per_round as f64;

    println!(
        "bench {name} | rounds={n} ops_per_round={ops} | min={min:.3} p50={p50:.3} p95={p95:.3} mean={mean:.3} us/op | {ops_per_sec:.0} ops/s",
        name = cfg.name,
        ops = cfg.ops_per_round,
        min = op_us(samples[0]),
        p50 = op_us(percentile(samples, 50.0)),
        p95 = op_us(percentile(samples, 95.0)),
        mean = op_us(mean),
        ops_per_sec = ops_per_sec,
    );
}

/// One line describing the machine the numbers came from.
///
/// CPU model comes from `PROCESSOR_IDENTIFIER` on Windows; everywhere
/// else it falls back to the arch string rather than inventing a name.
pub fn print_platform() {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpu = std::env::var("PROCESSOR_IDENTIFIER")
        .unwrap_or_else(|_| std::env::consts::ARCH.to_string());
    println!(
        "platform os={} arch={} cpus={} cpu=\"{}\"",
        std::env::consts::OS,
        std::env::consts::ARCH,
        cpus,
        cpu
    );
}

/// A throwaway root directory under the OS temp dir, removed on drop.
pub struct TempRoot {
    path: std::path::PathBuf,
}

impl TempRoot {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "cachegirl-bench-{tag}-{}-{nanos:x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("benchmark root must be creatable");
        Self { path: dir }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Keep the optimizer honest around measured calls; results flow
/// through here so nothing gets constant-folded away.
#[inline(always)]
pub fn observed<T>(value: T) -> T {
    black_box(value)
}
