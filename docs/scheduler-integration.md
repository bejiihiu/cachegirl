# Scheduler integration

`cachegirl` deliberately has no scheduler. It owns no timer, no thread
and no runtime; it exposes `cleanup_expired()` and gets out of the way.
That is a design decision, not a missing feature: when a sweeper is
embedded in a library, the library inherits the application's runtime
assumptions — async runtimes, executor flavors, thread pinning,
shutdown ordering — and forces them on every embedder. Instead, you
call the sweep from wherever your application already has a sense of
time.

The sweep is explicit and cheap to reason about: it takes one pass over
the registry and returns a report. Whatever calls it is entirely your
choice.

## Pattern 1 — call it inline from a control point

The simplest option: run the sweep at a place where the application
already does periodic work (end of a request, a health check, a batch
boundary).

```rust
use cachegirl::{CreateOptions, TempManager};
use std::time::Duration;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-inline-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;

    let ns = manager.namespace("jobs")?;
    ns.create_file_with(
        "intermediate.bin",
        CreateOptions::new().ttl(Duration::from_millis(1)),
    )?;
    std::thread::sleep(Duration::from_millis(20));

    // "the application decided it is time" — one line, no extra machinery
    let report = manager.cleanup_expired()?;
    println!("inline sweep: removed={}", report.removed);

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Inline sweeps are fine for low-frequency riders (startup, shutdown,
cron-like batch boundaries). For anything hotter, dedicate a timer.

## Pattern 2 — a std::thread with an interval

Exactly what `examples/custom_scheduler.rs` does: one dedicated thread,
an atomic stop flag, `std::thread::sleep` between ticks, graceful join
on shutdown.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cachegirl::{CreateOptions, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-sched-{}", std::process::id()));
    let manager = Arc::new(
        TempManager::builder()
            .root(&dir)
            .cleanup_on_shutdown(true)
            .build()?,
    );

    let ns = manager.namespace("work")?;
    ns.create_file_with("short-lived.bin", CreateOptions::new().ttl(Duration::from_secs(1)))?;

    let stop = Arc::new(AtomicBool::new(false));
    let scheduler = {
        let manager = Arc::clone(&manager);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match manager.cleanup_expired() {
                    Ok(report) => {
                        if report.removed > 0 {
                            println!("sweep reclaimed {} resource(s)", report.removed);
                        }
                    }
                    Err(e) => eprintln!("sweep failed: {e}"),
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };

    std::thread::sleep(Duration::from_millis(1100));
    stop.store(true, Ordering::Relaxed);
    scheduler.join().expect("scheduler thread joins");

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

The whole scheduler — spawn, loop, stop, join — is twenty lines of
application code. No library dependency on a runtime, no hidden worker.

## Pattern 3 — tokio interval

With the `tokio` feature, the async facade's `cleanup_expired` offloads
the pass to the blocking pool, so it does not stall the executor:

```rust
use cachegirl::AsyncTempManager;
use cachegirl::CreateOptions;
use std::time::Duration;

#[tokio::main]
async fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-tokio-{}", std::process::id()));
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .cleanup_on_shutdown(true)
        .build()
        .await?;

    let ns = manager.namespace("work").await?;
    ns.create_file_with("short-lived.bin", CreateOptions::new().ttl(Duration::from_secs(1)))
        .await?;

    let mut interval = tokio::time::interval(Duration::from_millis(100));
    for _ in 0..12 {
        interval.tick().await;
        let report = manager.cleanup_expired().await?;
        if report.removed > 0 {
            println!("sweep reclaimed {} resource(s)", report.removed);
            break;
        }
    }

    manager.shutdown().await?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Note: `#[tokio::main]` with default flavor is multi-thread; the same
snippet works on `current_thread`. Under the `tokio` feature, the
sweep runs on the blocking pool; without it (plain `async`), it runs
inline on the calling thread.

## Pattern 4 — external schedulers (cron, systemd, Kubernetes)

If the application is a CLI or a batch job, the scheduler already exists
outside the process. Two shapes work:

- **Sweep inside the process** — the process lives long enough to be
  its own sweeper (a daemon with a `tokio::time::interval` or a
  `std::thread`, as above).
- **Sweep as a separate invocation** — e.g. a cron entry runs
  `myapp cachegirl-sweep`, which builds a manager (`cleanup_on_start`
  reclaims whatever the previous crashed run left), runs one
  `cleanup_expired()`, prints the report and exits. Recovery does the
  heavy lifting for crashed instances; the sweep does it for live ones.

For systemd users, a timer unit (`OnUnitActiveSec=`) plus a small CLI
subcommand is the idiomatic integration. Nothing in the crate prevents
or requires any of this.

## Pattern 5 — event-driven

If your application already has a busy-loop boundary with natural
quiescence (a game server tick, a data pipeline's flush point, an
actor's mailbox drained), call the sweep there. The trigger should be
cheap to detect (queue emptied, frame budget with slack, request
arrival when idle) — the sweep itself is the expensive part.

## Choosing per architecture

| application           | suggested pattern                                            |
| --------------------- | ------------------------------------------------------------ |
| web service           | `tokio::time::interval` every 1–60 s, or sweep on shutdown   |
| CLI / batch           | inline sweep at the end, or a cron-like wrapper invocation    |
| game server           | sweep on the idle tick; handle drops on the main thread      |
| data pipeline         | sweep at batch boundaries; `Managed` resources everywhere     |
| worker daemon         | dedicated `std::thread` with a 100 ms–10 s interval          |

## Rules of thumb

- Sweep frequency bounds garbage growth, not correctness — quotas are
  enforced at creation regardless.
- A sweep is O(registry size); at 10k records it measured ~4.3 us/op
  per record on the machine documented in [benchmarks.md](benchmarks.md).
  Do not run it in a hot loop with no reason to.
- Overlap is safe: two concurrent sweeps serialize on the internal
  locks, and the loser of each destroy race reports `skipped` — a
  needle that is not a correctness problem. Still, avoid scheduling
  sweeps from multiple places if you can help it; one timer, one owner.
- The sweep is explicit by design; if you never call it, resources only
  disappear through explicit `release` or owning-drop, never through
  time.
