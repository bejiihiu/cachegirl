# Observability

`cachegirl` reports through three channels: statistics snapshots,
structured events behind the `EventSink` trait, and (optionally) the
`metrics` and `tracing` facades. None of the optional channels is ever
installed by default, and the default build pays nothing for them.

## Statistics snapshots

`manager.stats()` returns a `Statistics` value — plain data,
cloneable, and serializable with the `serde` feature:

| field              | meaning                                             |
| ------------------ | --------------------------------------------------- |
| `instance_id`      | which instance this snapshot describes               |
| `captured_at`      | `Option<SystemTime>` of the snapshot                |
| `namespaces`       | live namespaces (created so far)                    |
| `resources`        | live registered resources                            |
| `bytes_tracked`    | sum of recorded sizes                                |
| `expired`          | records in `Expired` state                          |
| `pending_cleanup`  | records in `PendingCleanup` (deferred by leases)    |
| `gc_runs`          | completed cleanup passes                             |
| `resources_released`| resources destroyed since the process started        |
| `bytes_freed`      | bytes reclaimed since the process started            |
| `active_leases`    | leases currently held                                |

`Statistics::render()` formats the snapshot as a fixed human-readable
block — useful for a diagnostics endpoint or an incident log line. It
is multi-line and intentionally unstructured.

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-obs-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let file = ns.create_file("probe.bin")?;
    let stats = manager.stats()?;
    println!("{}", stats.render());

    ns.release(file.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

## Events: EventSink

Everything notable that happens inside the manager is emitted as an
`Event`:

- `ResourceCreated { instance, id, kind, namespace }`
- `ResourceReleased { instance, id, bytes_freed }`
- `CleanupCompleted { instance, scanned, removed, failed, bytes_freed }`
- `RecoveryCompleted { instance, scanned, removed, kept, failed, bytes_freed }`

An `EventSink` is a trait with one method, `emit(&self, event: &Event)`.
Sinks are called **synchronously from the operation that emits them**,
including from destructors (`ResourceReleased` fires from inside the
drop path for `Owned`/`Shared` resources). Two hard requirements
follow: keep `emit` cheap, and never panic — a panicking sink aborts
the surrounding operation and, during destruction, can leave the
resource's bookkeeping in an unknown state (the physical side may
already be gone).

Three sinks ship with the crate:

| sink         | feature   | purpose                                    |
| ------------ | --------- | ------------------------------------------ |
| `VecSink`    | —         | records events in memory; for tests        |
| `NullSink`   | —         | the default; drops everything              |
| `TracingSink`| `tracing` | re-emits events as `tracing::debug!` records |

```rust
use std::sync::Arc;
use cachegirl::{Event, EventSink, TempManager, VecSink};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-sink-{}", std::process::id()));
    let sink = Arc::new(VecSink::new());
    let manager = TempManager::builder()
        .root(&dir)
        .event_sink(Arc::clone(&sink) as Arc<dyn EventSink>)
        .build()?;

    let ns = manager.namespace("demo")?;
    let file = ns.create_file("probe.bin")?;
    ns.release(file.id())?;

    for event in sink.drain() {
        match event {
            Event::ResourceCreated { kind, namespace, .. } => {
                println!("created kind={kind} namespace={namespace}")
            }
            Event::ResourceReleased { bytes_freed, .. } => {
                println!("released bytes_freed={bytes_freed}")
            }
            other => println!("{other:?}"),
        }
    }

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

## Tracing (feature `tracing`)

Operations are instrumented with spans, and lifecycle steps are logged
as events. With the feature disabled, every span and event compiles to
nothing.

Spans (`#[instrument]` attributes):

| span              | level | fields                              |
| ----------------- | ----- | ----------------------------------- |
| `create_resource` | debug | `namespace`, `kind`, `name`         |
| `destroy_resource`| debug | `id`, `origin`                     |
| `cleanup_pass`    | info  | `scope`                             |
| `shutdown`        | info  | `instance`                          |
| `recovery`        | info  | `instance`                          |

Events (as `tracing::...` calls): creation success/failure (`debug` /
`warn`), release success/failure (`debug` / `error`), lease
acquire/release/deferred (`debug`), quota rejection (`warn`), namespace
created (`debug`), cleanup completed (`info`), recovery completed
(`info`, or `warn` when `failed > 0`), shutdown requested/completed
(`info`). Field names match the event payload where applicable
(`namespace`, `kind`, `id`, `bytes_freed`, `scanned`, `removed`,
`failed`, `deferred`, `scope`, `duration_ms`, ...).

`TracingSink` additionally turns sink events into structured debug
records — enable it to see the `Event` stream through your subscriber:

```rust
use std::sync::Arc;
use cachegirl::{TempManager, TracingSink};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-tracing-{}", std::process::id()));
    let manager = TempManager::builder()
        .root(&dir)
        .event_sink(Arc::new(TracingSink::new()) as Arc<dyn cachegirl::EventSink>)
        .build()?;

    let ns = manager.namespace("demo")?;
    let file = ns.create_file("probe.bin")?;
    ns.release(file.id())?;

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

This snippet requires the `tracing` feature.

## Metrics (feature `metrics`)

Instrumentation goes through the `metrics` facade; the crate never
installs a recorder — your application picks the exporter and
wires it up before building the manager. All metric names are fixed:

| metric | kind | unit | labels |
| ------ | ---- | ---- | ------ |
| `cachegirl_resources_created_total` | counter | — | `namespace`, `kind`, `result` |
| `cachegirl_resources_released_total` | counter | — | `namespace`, `kind`, `result` |
| `cachegirl_bytes_freed_total` | counter | bytes | — |
| `cachegirl_active_resources` | gauge | — | — |
| `cachegirl_active_leases` | gauge | — | — |
| `cachegirl_cleanup_runs_total` | counter | — | `scope` |
| `cachegirl_cleanup_duration_seconds` | histogram | seconds | `scope` |
| `cachegirl_quota_rejections_total` | counter | — | `namespace`, `reason` |
| `cachegirl_deferred_lease_operations_total` | counter | — | `operation` |

Label values: `result` is `ok` or `error`; `scope` is `manager` or
`namespace`; `kind` is the lowercase resource kind
(`file`, `directory`, `buffer`, `cache`, `serialization`, `runtime`,
`custom`); `reason` is `size`, `count` or `namespace`; `operation` is
`release`, `cleanup` or `drop`.

Two implementation notes worth knowing:

- `cachegirl_active_resources` and `cachegirl_active_leases` are
  maintained as delta gauges shared by every manager in the process
  (not per `instance_id`); use a recorder that tolerates that, or
  export to a system where a process-wide gauge is fine.
- `StatisticsCollector::emit_metrics(instance_id)` dumps the collector's
  raw counters as instance-labeled **gauges**:
  `cachegirl_gc_runs_total{instance}`,
  `cachegirl_resources_released_total{instance}` and
  `cachegirl_bytes_freed_total{instance}`. Those duplicate the two
  unlabeled counters under different label sets — pick one style, or
  expect double counting in naive pipelines.

An in-memory recorder is enough for tests, and matches how the
integration suite verifies emission. The `metrics_usage.rs` example
ships a small working recorder.

## CleanupReport as an observability surface

The per-pass report is the cheapest signal you have: wire it into your
own metrics (`report.removed`, `report.failed`, `report.deferred`,
`report.bytes_freed`, `report.reasons.*`) from the code that calls
`cleanup_expired`, and alert on `failed > 0` or a growing
`deferred`. The library tracks and reports but never decides policy —
what a non-zero `deferred` means for your SLOs is your call.

## What is deliberately not emitted

- No prometheus endpoint, no HTTP surface, no exporter. Install one
  yourself behind a flag if you need it.
- No per-operation histograms beyond cleanup pass duration.
- No logs by default — no subscriber is installed and no output is
  printed unless you enable `tracing` and register one.
