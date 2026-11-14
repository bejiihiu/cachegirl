# Performance

This page records the architecture that determines performance, the
measured numbers available today, and the tuning levers that actually
move them. The numbers come from one machine; rerun the suite on yours
before making capacity decisions.

## Architecture: where the time goes

- **One mutex on the registry.** Every lookup, register, update and
  remove serializes on it. Quota checks must be atomic with
  registration, so creation takes the creation lock too, which
  serializes all creates manager-wide regardless of namespace.
- **No background threads.** Nothing burns CPU between your calls.
  Cost is paid at creation, release and sweep time only.
- **Creation is serialized by design.** The quota check + registry
  registration happen under one lock; concurrent creators queue. The
  concurrent bench exists to measure how hard that contention can be
  pushed (see below), not to showcase parallelism.
- **The cleanup pass clones every registry record for its scan.** Cost
  grows superlinearly with registry size; at 10k records it is ~4× the
  per-record cost of 1k records. Reducing registry size (small quotas,
  frequent sweeps) keeps sweeps flat.
- **`refresh_size` is a `stat` syscall.** It is not bookkeeping on
  integers; it is the filesystem asking the kernel. Do not call it
  "to be safe" on hot paths.

## Measured numbers

Machine: Windows, x86_64, 16 logical CPUs (Intel Family 6 Model 186
Stepping 2), rustc 1.99.1, bench profile, 2026-11-14. All features.
Full methodology: [benchmarks.md](benchmarks.md).

```text
bench allocation/buffer_create_release | p50=2.362 p95=2.406 us/op | 423970 ops/s
bench allocation/buffer_drop_owned     | p50=2.022 p95=2.175 us/op | 489570 ops/s
bench allocation/file_create_release   | p50=280.670 p95=365.828 us/op | 3368 ops/s
bench packet/buffer_64kb_create_use_release | p50=2.748 p95=4.780 us/op | 330288 ops/s
bench cache/churn_1000_keys            | p50=3.469 p95=3.530 us/op | 288256 ops/s
bench cleanup/expired_100              | p50=1.055 p95=1.090 us/op | 947867 ops/s
bench cleanup/expired_1000             | p50=1.084 p95=1.155 us/op | 950764 ops/s
bench cleanup/expired_10000            | p50=4.232 p95=4.516 us/op | 231288 ops/s
bench concurrent/8x250_buffer_create_release | p50=4.063 p95=11.208 us/op | 193993 ops/s
bench registry/register_get_remove     | p50=0.511 p95=0.534 us/op | 1927934 ops/s
```

Reading them:

- **Buffers are free-ish.** ~2.4 µs per create+release pair is the
  baseline every other number should be compared against. The owning
  drop path is measured slightly cheaper (2.0 µs) than explicit
  `release` (2.4 µs) — `release` pays one extra registry lookup before
  delegating to the same destroy machinery.
- **Files cost 100× a buffer.** ~297 µs per create+release is
  filesystem syscalls (create + metadata + remove), not bookkeeping.
  If your algorithm creates a file per packet, the file is the budget.
- **Cleanup is flat until ~1k records**, then grows. The 10k number is
  the one to watch if registries grow: the pass copies every record to
  compute the scan. Smaller `max_resources` and more frequent sweeps
  keep the shape flat.
- **8-thread creation does not scale** (~194k ops/s wall vs ~424k
  single-threaded buffers). The registry lock is the serializer; more
  namespaces do not help because the lock is manager-wide.

## Recommendations

**Buffer reuse over churn.** `create_buffer` pre-allocates capacity
and accounts it; a hot path that creates and drops 64 KiB buffers at a
high rate should instead pool them: `BufferPool::acquire` keeps a fixed
set of allocations in circulation (checkout/return, no registry
bookkeeping at all), or hold one buffer per worker and `clear()` +
rewind it. Pooled buffers are unaccounted scratch space — accounting
still applies to tracked resources created through the namespace API.
Buffer churn is the single biggest lever on memory-bound workloads.

**`refresh_size` deliberately.** Call it after you know the file
changed (finish of a write), not on read. It is the only O(stat) call
in the API; everything else is registry arithmetic.

**Sweep at batch boundaries.** One sweep per 1,000 operations is not
the same cost as one sweep after every operation, and quotas hold
between sweeps regardless. Batch sweeps also give the failure
accounting a chance to amortize.

**Namespace-shard for isolation, not speed.** Splitting namespaces
buys independent quotas, independent sweeps and blast-radius control;
it does not buy creation parallelism (the registry lock is
manager-wide). Do not shard "for throughput".

**`Managed` + explicit release on async hot paths.** `Owned` drops do
I/O inline on the executor; switching the resource to `Managed` and
releasing on the blocking pool keeps the hot path free of filesystem
latency.

**Reduce registry pressure.** Quota eviction scans the namespace;
sweep cost grows with registry size. Small `max_resources` limits,
low TTLs on scratch data, and namespace-scoped sweeps for hot
namespaces all keep the per-record cost at the ~1 µs plateau.

## What is not optimized

- **The 10k+ record scan.** The pass clones every registry record to
  classify it; this is the known superlinear term. If you operate with
  five-digit registries as a steady state, this is the number to
  benchmark on your machine and possibly the one to file an issue
  about.
- **File creation.** Nothing to tune here; it is syscalls.
- **Cross-process contention.** Two managers in one process pay the
  same lock cost; two machines do not coordinate at all. There is no
  distributed lock and no need for one.
- **`Statistics::render()`** allocates and formats; it is for
  diagnostics, not for per-request paths.

## Measuring it yourself

```sh
cargo bench            # default features
cargo bench --all-features
```

The suite uses a std-only timing harness (no criterion dependency);
percentiles are min/p50/p95/mean over per-round samples. Compare
inter-run relative deltas, not absolute numbers across machines, and
be aware that the `concurrent` bench on a busy machine is mostly a
measure of scheduler behavior.
