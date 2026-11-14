# cachegirl

Centralized management of temporary resources in Rust.

`cachegirl` gives your application a single registry for temporary
files, directories, in-memory buffers, cached computation results, and
other short-lived data — with namespaces, quotas, leases, cleanup
policies, and crash recovery. It never spawns background threads,
never requires an async runtime, and never imposes a filesystem layout:
you choose the root, the namespace structure, and when cleanup runs.

## Feature flags

| feature      | default | what it enables                          |
|--------------|---------|------------------------------------------|
| `filesystem` | yes     | filesystem backend and file resources    |
| `memory`     | no      | memory backend and buffer resources      |
| `cache`      | no      | cache entry resources                    |
| `async`      | no      | async API markers                        |
| `sync`       | no      | sync API markers                         |
| `serde`      | no      | `Serialize`/`Deserialize` for public types |
| `metrics`    | no      | counter export through the `metrics` facade |
| `tracing`    | no      | event emission through `tracing`         |
| `tokio`      | no      | Tokio integration (implies `async`)      |

## Quick start

```toml
[dependencies]
cachegirl = { version = "0.1" }
```

```rust
use cachegirl::TempManager;

let manager = TempManager::builder()
    .root("./runtime/tmp")
    .create_if_missing(true)
    .build()?;

let protocol = manager.namespace("protocol")?;
// ... create and use resources ...
let report = manager.cleanup_expired()?;
manager.shutdown()?;
```

## Repository

<https://github.com/bejiihiu/cachegirl>

## License

MIT OR Apache-2.0
