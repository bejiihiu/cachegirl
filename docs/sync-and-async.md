# Sync and async

The crate has one implementation: a synchronous core that works without
any runtime and without spawning threads. On top of it sits an async
facade — `AsyncTempManager` / `AsyncNamespace` — that forwards every
call into the core. The facade changes *where* the blocking work
happens, never *what* it does.

## The execution model, precisely

| build                     | filesystem / backend work | memory work        | notes                          |
| ------------------------- | ------------------------- | ------------------ | ------------------------------ |
| default (`filesystem`)    | on the calling thread     | on the calling thread | exactly the sync behavior   |
| `async` only              | on the calling thread     | on the calling thread | async signatures, inline execution |
| `tokio` + ambient runtime | `tokio::task::spawn_blocking` | on the calling thread | executor never blocked        |
| `tokio`, no runtime       | on the calling thread     | on the calling thread | the inline fallback; no panic  |

"Filesystem / backend work" means: manager build (startup recovery),
namespace creation (`mkdir`), `create_file`/`create_directory`,
`create_custom`, `release`, `refresh_size`, `cleanup_expired`,
`shutdown`. "Memory work" means buffers, serialization payloads, cache
entries, registry bookkeeping and lease handling, which never leave the
calling thread.

If the offload future is cancelled or the blocking closure panics, the
error surfaces as `Error::Concurrency(ConcurrencyError::Cancelled)` or
`Error::Concurrency(ConcurrencyError::BlockingTaskPanic(..))` — never
as a silently missing file or a stuck manager.

## A tokio program

```rust
use cachegirl::AsyncTempManager;

#[tokio::main]
async fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-async-{}", std::process::id()));
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("example-instance")
        .cleanup_on_shutdown(true)
        .build()
        .await?;

    let protocol = manager.namespace("protocol").await?;

    let file = protocol.create_file("packet-dump.bin").await?;
    std::fs::write(file.path(), b"\x00\x01\x02\x03").expect("write packet dump");
    let size = protocol.refresh_size(file.id()).await?;
    println!("tracked {size} bytes");

    let file_id = file.id().clone();
    protocol.release(&file_id).await?;

    let report = manager.cleanup_expired().await?;
    println!("cleanup: scanned={} removed={}", report.scanned, report.removed);

    manager.shutdown().await?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Under the `tokio` feature, every `.await?` that touches the disk goes
to the blocking pool; without Tokio everything runs inline. Behavior —
events, statistics, quotas, registry transitions — is identical either
way.

## Async without a runtime

With the `async` feature alone, there is nothing to set up; the futures
resolve immediately on the calling thread. Driving them is the caller's
job — any executor, including a hand-rolled one:

```rust
use cachegirl::AsyncTempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-norun-{}", std::process::id()));

    let future = async {
        let manager = AsyncTempManager::builder().root(&dir).build().await?;
        let ns = manager.namespace("protocol").await?;
        assert_eq!(ns.name(), "protocol");
        manager.shutdown().await?;
        Ok::<(), cachegirl::Error>(())
    };

    // any executor will do; this is the smallest one imaginable
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(result) => {
                result?;
                break;
            }
            std::task::Poll::Pending => std::thread::yield_now(),
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

This compiles with the `async` feature alone, and also with `tokio`
enabled (the inline fallback kicks in because no runtime is ambient).

## What stays synchronous, and why

Resource handles are deliberately **not** wrapped in async traits.
`Drop` cannot be async, and `Owned`/`Shared` destruction does I/O
inline. Consequences:

- dropping an owning handle on an executor does blocking work on that
  thread — wrap drops in `spawn_blocking`, or use `Managed` ownership
  and release explicitly via the facade;
- `handle.acquire()` is atomic bookkeeping in the lease book — no
  filesystem, no await;
- `FileResource::len()` and `modified()` call `fs::metadata` directly —
  in async code prefer `AsyncNamespace::refresh_size` or a blocking
  pool call.

## Escape hatches: into_inner / as_inner

`AsyncTempManager::into_inner()` hands back the shared `Arc<TempManager>`
(no copies are made — it clones the `Arc`); `as_inner()` borrows it.
`AsyncNamespace::into_inner()` / `as_inner()` do the same for
namespaces. Use them to call a sync-only API (for example something
you wrote) from an async context without giving up the shared state.
Nothing about the manager is closed off by the facade; it is an
adapter, not a wall.

## Feature reference

| feature | default | pulls in | purpose |
| ------- | ------- | -------- | ------- |
| `filesystem` | yes | — | filesystem backend, file/directory resources, recovery |
| `memory` | no | — | memory backend, buffer/serialization/runtime resources |
| `cache` | no | — | cache entry resources (creation requires `memory`) |
| `async` | no | — | the async facade, runtime-agnostic |
| `sync` | no | — | reserved marker; sync API is always available |
| `tokio` | no | `tokio`, `async` | blocking operations on the blocking pool |
| `serde` | no | `serde` | `Serialize`/`Deserialize` for data-shaped types |
| `metrics` | no | `metrics` | counters/gauges/histograms via the facade |
| `tracing` | no | `tracing` | spans and structured events |

`sync` is a reserved marker with no effect today. If your build system
requires a sync feature to exist, enabling it is a no-op; the sync API
is always present.

## Which to pick

- Plain sync application, one runtime or none: the sync API. Zero
  abstraction, zero await points, nothing to configure.
- Axum/actix/tokio service: the async facade with `tokio`, and drop
  owning handles on the blocking pool or use `Managed` + explicit
  release.
- Mixed: both are always available together; they share state through
  the same `Arc`s.
