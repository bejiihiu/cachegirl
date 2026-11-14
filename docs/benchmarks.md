# Benchmarks

This document records the current state of `cachegirl`'s benchmark suite and
the numbers measured on one machine. The suite exists to make regressions
visible and to give capacity planning a defensible starting point; it is not
a claim about any other hardware.

## How to run

```sh
cargo bench --all-features
```

Plain `cargo bench` builds and runs what the default feature set supports;
benches that need `memory`/`cache` print a one-line skip note instead of
failing. Every bench target is a `harness = false` binary that times with
`std::time::Instant` — the suite has no external benchmark dependency, so it
builds on any target, including windows-gnu without `dlltool`, where
FFI-linking benchmark crates do not build.

## Methodology

- Each measurement series runs a fixed number of warmup rounds, then a fixed
  number of measured rounds. Per-round wall times are the samples.
- Reported percentiles (min, p50, p95, mean) are nearest-rank statistics over
  the sorted per-round samples, converted to us/op by dividing by the op
  count of a round.
- ops/sec is total measured work over total measured time, not a property of
  any single sample.
- `setup` work (creating expired resources, building the manager) runs
  outside the timed window; the timed part is exactly the operation being
  measured.
- Source: `benches/`.

## Machine used for the numbers below

- OS: `windows` (as reported by `std::env::consts::OS`), arch `x86_64`
- CPUs: 16 logical (as reported by `std::thread::available_parallelism`)
- CPU (from `PROCESSOR_IDENTIFIER`): `Intel64 Family 6 Model 186 Stepping 2, GenuineIntel`
- rustc 1.99.1, `bench` profile (release-equivalent), all features enabled
- 2026-11-14

## Results

```text
platform os=windows arch=x86_64 cpus=16 cpu="Intel64 Family 6 Model 186 Stepping 2, GenuineIntel"
bench allocation/buffer_create_release | rounds=20 ops_per_round=500  | min=2.306 p50=2.362 p95=2.406 mean=2.359 us/op | 423970 ops/s
bench allocation/buffer_drop_owned     | rounds=20 ops_per_round=500  | min=1.999 p50=2.022 p95=2.175 mean=2.043 us/op | 489570 ops/s
bench allocation/file_create_release   | rounds=10 ops_per_round=100  | min=269.402 p50=280.670 p95=365.828 mean=296.918 us/op | 3368 ops/s
bench packet/buffer_64kb_create_use_release | rounds=50 ops_per_round=200 | min=2.614 p50=2.748 p95=4.780 mean=3.028 us/op | 330288 ops/s
bench cache/churn_1000_keys             | rounds=10 ops_per_round=1000 | min=3.409 p50=3.469 p95=3.530 mean=3.469 us/op | 288256 ops/s
bench cleanup/expired_100               | rounds=10 ops_per_round=100  | min=1.011 p50=1.055 p95=1.090 mean=1.055 us/op | 947867 ops/s
bench cleanup/expired_1000              | rounds=7  ops_per_round=1000 | min=0.920 p50=1.084 p95=1.155 mean=1.052 us/op | 950764 ops/s
bench cleanup/expired_10000             | rounds=5  ops_per_round=10000 | min=4.175 p50=4.232 p95=4.516 mean=4.324 us/op | 231288 ops/s
bench concurrent/8x250_buffer_create_release | rounds=10 ops_per_round=2000 | min=3.855 p50=4.063 p95=11.208 mean=5.155 us/op | 193993 ops/s
bench registry/register_get_remove      | rounds=20 ops_per_round=1000 | min=0.510 p50=0.511 p95=0.534 mean=0.519 us/op | 1927934 ops/s
```

Benchmark definitions:

| Benchmark | Workload per op |
|---|---|
| `allocation/buffer_create_release` | `create_buffer(4096)` + `release` |
| `allocation/buffer_drop_owned` | `create_buffer_with(4096, Owned)` + drop (destroy via the `Drop` path) |
| `allocation/file_create_release` | `create_file` + `release` on a real filesystem under the OS temp dir |
| `packet/buffer_64kb_create_use_release` | create 64 KiB buffer, write 64 KiB, read back, release |
| `cache/churn_1000_keys` | one full sweep of a 1,000-key namespace: `create_cache_entry` (1 KiB) → `cache_get` → `release` |
| `cleanup/expired_*` | one `cleanup_expired()` pass over N expired (TTL backdated) managed buffers |
| `concurrent/8x250_buffer_create_release` | wall-time round with 8 workers x 250 buffer create+release pairs each |
| `registry/register_get_remove` | one register → get → remove triple on a fresh record |

## Observations

- The `Owned` drop path is not more expensive than explicit `release` —
  measured slightly cheaper here (2.0 vs 2.4 us/op). `release` pays an extra
  `registry.get` (a metadata clone) before delegating to the same destroy
  machinery the `Drop` path uses.
- File create+release is two orders of magnitude more expensive than buffer
  create+release (~297 us/op vs ~2.4), dominated by filesystem syscalls.
- `cleanup_expired()` stays flat around 1 us/op between 100 and 1,000
  expired resources, then degrades to ~4.3 us/op at 10,000 — the pass clones
  every registry record for its scan, so cost grows superlinearly with the
  registry size. Worth optimizing if large registries become common.
- The 8-thread concurrent run does not scale past the single-threaded
  number (~194k ops/s wall). Creation serializes on the registry's internal
  mutex by design (quota checks must be atomic), so this bench mainly
  measures how hard that contention can be pushed, not peak parallel
  throughput.
- Numbers are from one machine at one moment; rerun on yours before making
  capacity decisions. CI/regression comparisons should use inter-run
  relative deltas, not these absolute values.
