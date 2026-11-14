# Resources

`cachegirl` manages seven categories of resource. Six of them are
built in (file, directory, buffer, cache entry, serialization
intermediate, runtime marker); the seventh is whatever you plug in
through a custom backend. All of them share one registry, one set of
quotas and one ownership model — the difference is only which backend
materializes and destroys them.

## The resource kinds

| kind           | handle type             | backend      | feature      | bytes known at creation |
| -------------- | ----------------------- | ------------ | ------------ | ----------------------- |
| `File`         | `FileResource`          | `filesystem` | `filesystem` (default) | no — track via `refresh_size` |
| `Directory`    | `DirectoryResource`     | `filesystem` | `filesystem` (default) | no — `refresh_size`      |
| `Buffer`       | `BufferResource`        | `memory`     | `memory`     | yes (capacity)          |
| `Cache`        | `CacheEntryResource`    | `memory`     | `cache` (implies `memory`) | yes                  |
| `Serialization`| `SerializationResource` | `memory`     | `memory`     | yes                     |
| `Runtime`      | `RuntimeResource`       | `memory`     | `memory`     | no (zero)               |
| `Custom`       | `CustomResource`        | your backend | —            | as reported by backend  |

Every handle is cheap to clone (`Arc` inside), carries the registry id
(`id()`), the ownership core, and an `acquire()` method for leases.
Cloning a handle clones co-ownership in `Shared` mode and a plain alias
in the other modes — it never creates a new registry record.

## Creating resources

```rust
use cachegirl::CreateOptions;
use std::time::Duration;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-res-{}", std::process::id()));
    let manager = cachegirl::TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    // filesystem resources (default features)
    let file = ns.create_file("packet.bin")?;
    let with_opts = ns.create_file_with(
        "packet-with-ttl.bin",
        CreateOptions::new().ttl(Duration::from_secs(60)),
    )?;
    let directory = ns.create_directory("staging")?;

    println!("file: {}", file.path().display());
    println!("dir:  {}", directory.path().display());
    println!("ttl:  {:?}", manager.registry().get(with_opts.id())?.ttl);

    ns.release(file.id())?;
    ns.release(with_opts.id())?;
    ns.release(directory.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

With `--features cache` (which implies `memory`) all of the in-memory kinds are available:

```rust
use cachegirl::CreateOptions;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-mem-{}", std::process::id()));
    let manager = cachegirl::TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let buffer = ns.create_buffer(64 * 1024)?;
    println!("buffer capacity: {}", buffer.capacity());

    let serialized = ns.create_serialization("frame-1", vec![1, 2, 3, 4])?;
    println!("serialization bytes: {}", serialized.len());

    let runtime = ns.create_runtime_with("req-42", CreateOptions::new().owner("billing"))?;
    println!("runtime marker: {}", runtime.name());

    let entry = ns.create_cache_entry("pkt-1", vec![0xde, 0xad, 0xbe, 0xef])?;
    let fetched = ns.cache_get("pkt-1")?.expect("entry must be cached");
    println!("cache hit: {} bytes", fetched.value.len());

    ns.release(buffer.id())?;
    ns.release(serialized.id())?;
    ns.release(runtime.id())?;
    ns.release(entry.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

This second snippet requires the `cache` feature (which implies
`memory`); the first one is default-feature clean.

## CreateOptions

`CreateOptions` is the single options object for creation. All fields
are optional; `CreateOptions::new()` is empty.

| option      | meaning                                                        |
| ----------- | -------------------------------------------------------------- |
| `owner`     | free-form component/request tag, recorded in the registry      |
| `ttl`       | fixed TTL counted from creation; overrides `default_ttl`       |
| `idle_ttl`  | sliding TTL counted from last access; mutually exclusive with `ttl` |
| `ownership` | `Owned` / `Managed` / `Shared` / `Leased`; default `Managed`  |
| `priority`  | eviction priority; falls back to the effective policy priority |
| `metadata`  | free-form key/value map, recorded in the registry record       |

Rules worth remembering:

- `ttl` and `idle_ttl` together are a configuration error
  (`ConfigError::Invalid`), even though both compile.
- Setting both through one builder chain is the common mistake.
- `ttl`/`idle_ttl` land in the record as `Ttl::Fixed`/`Ttl::Idle`;
  the registry, the cleanup engine and reports all see the same value.
- `priority` interacts with eviction: lower priority is evicted first
  when a quota is exceeded (`Low < Normal < High < Critical`). If unset,
  the resource inherits the priority of the effective cleanup policy
  (namespace-specific if set, then manager-wide, else `Normal`).

```rust
use cachegirl::{CreateOptions, Ownership, Priority};
use std::time::Duration;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-opts-{}", std::process::id()));
    let manager = cachegirl::TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let file = ns.create_file_with(
        "important.bin",
        CreateOptions::new()
            .owner("billing-worker")
            .ttl(Duration::from_secs(300))
            .ownership(Ownership::Managed)
            .priority(Priority::High)
            .metadata("request", "req-42"),
    )?;
    let meta = manager.registry().get(file.id())?;
    println!("owner={:?} ttl={:?} priority={:?}", meta.owner, meta.ttl, meta.priority);
    println!("request={}", meta.metadata.get("request").map(String::as_str).unwrap_or("?"));

    ns.release(file.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

## Paths and sizes

- Filesystem resources carry an absolute, canonicalized `path()`. Files
  are created with `create_new(true)` — creation never clobbers an
  existing file, and fails with `BackendError::Rejected` if the name is
  taken, which also protects against a hostile or broken caller
  overwriting data.
- Sizes are only known up front for memory-backed resources (capacity
  is the accounted size). Files are registered with `size_bytes = 0`;
  call `refresh_size(id)` to sync with `fs::metadata`. Cleanup reports
  and quota eviction use the recorded size, so an unsynced file is
  accounted as zero bytes. `refresh_size` is observational: a resource
  that grew past its quota re-stats successfully and stays tracked; it
  is reclaimed by quota eviction on a later cleanup pass, not by this
  call.
- `refresh_size` is a `stat` syscall; it is cheap but not free. Do not
  call it in a tight loop, and do not call it from async code
  expecting non-blocking behavior — use the async facade, which puts it
  on the blocking pool.
- Files and directories are owned by the concrete RAII handles
  (`FileResource`, `DirectoryResource`, and their memory-backed
  siblings); dropping the last owning handle releases the resource
  according to its `Ownership` mode.

## Pooled buffers

`BufferPool` (with the `memory` feature) recycles buffer allocations for
hot paths: `acquire` takes a cleared buffer out of the pool, and dropping
the returned `PooledBuffer` hands the allocation back for the next
checkout. Buffers that outgrew `max_buffer_capacity`, or buffers arriving
at a full pool, are dropped instead of retained. `BufferPool::new()`
keeps 64 buffers of at most 1 MiB each; `with_limits(max_buffers,
max_buffer_capacity)` tunes both knobs. Both limits accept zero — a pool
with `max_buffers == 0` never retains anything. `acquire_with_capacity`
hands out the smallest buffer that fits the request; with nothing
fitting, the smallest idle buffer is grown rather than stranded.

```rust
use cachegirl::BufferPool;

fn main() -> cachegirl::Result<()> {
    let pool = BufferPool::with_limits(16, 64 * 1024);
    {
        let mut buf = pool.acquire_with_capacity(4096);
        buf.extend_from_slice(b"reusable scratch space");
        assert!(buf.capacity() >= 4096);
        // dropping the handle returns the allocation to the pool
    }
    let reused = pool.acquire();
    assert!(reused.capacity() >= 4096);
    Ok(())
}
```

A pool is deliberately not tied to a namespace, a manager or the quota
system: pooled buffers are not registered resources, and routing them
through quotas would either permanently consume the quota the pool itself
sits on, or require quota-release plumbing on a `Drop` that cannot report
failure. If you need accounted, policy-managed buffers, use
`create_buffer`; the pool is its unaccounted sibling for scratch space
that never escapes the hot path. The pool is `Send`/`Sync` and safe to
share across threads; cloning it clones an `Arc` to the same arena.

## cache_get

With the `cache` feature, `cache_get(key)` returns the stored entry
(`Option<CacheEntry>`) and refreshes `last_accessed` both in the cache
store and in the registry record. That refresh is what makes an entry
survive idle-TTL eviction. Creating the same key twice in one namespace
is rejected with `BackendError::Rejected`; the store is scoped per
namespace, so two namespaces may reuse keys freely.

```rust
use cachegirl::CreateOptions;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-cache-{}", std::process::id()));
    let manager = cachegirl::TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let entry = ns.create_cache_entry_with(
        "greeting",
        b"hello".to_vec(),
        CreateOptions::new().metadata("kind", "test"),
    )?;
    println!("entry id: {}", entry.id());

    if let Some(got) = ns.cache_get("greeting")? {
        println!("cache hit (created at {:?})", got.created_at);
    }

    ns.release(entry.id())?;
    assert!(ns.cache_get("greeting")?.is_none(), "released entries disappear");
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Like the previous in-memory snippet, this one requires `--features
cache` (which implies `memory`).

## Runtime resources

`RuntimeResource` carries no bytes. It exists so the registry can track
lifetime-scoped objects that are not files and not buffers — a session,
an in-flight request, a worker slot — with the same quotas, leases,
statistics and cleanup machinery as everything else. It counts towards
`max_resources` per namespace and per manager and towards
`gc`/`release` statistics, but never towards byte quotas.

## Custom resources

`create_custom(name, backend)` and `create_custom_with(name, backend,
options)` route creation through a user-registered backend. The
`CustomResource` handle carries `backend()` with the backend's name. The
contract between the manager and custom backends is described in
[writing-a-backend.md](writing-a-backend.md).

## Resource lifecycle states

Every record moves through `ResourceState`: `Created`/`Active`/`Locked`
at creation and use, `Expired` when a TTL lapses or eviction nominates
it, `PendingCleanup` when destruction was requested but leases held,
`Destroyed` after the physical removal (the record is removed, not
marked), `FailedCleanup` when removal failed (retried with a budget of
`MAX_CLEANUP_ATTEMPTS`). Most applications never see states directly;
they surface in `ResourceMeta::state`, reports and statistics
(`expired`, `pending_cleanup`). The state machine rejects illegal
transitions with `RegistryError::InvalidTransition`.
