# Core concepts

`cachegirl` manages the temporary resources an application creates and
then forgets: scratch files, in-memory buffers, cached computation
results, serialization intermediates, runtime-scoped markers. This page
is the mental model behind the whole crate. Read it once, then use the
other guides as reference.

## The model: Application → Manager → Namespace → Resource

Every resource in the crate is owned by exactly one namespace, and
every namespace belongs to exactly one manager.

```text
Application
└── TempManager            (one root directory, one registry, one lock file)
    ├── Namespace "scratch"
    │   ├── FileResource     root/<instance>/scratch/packet.bin
    │   └── BufferResource   (in memory, accounted in the registry)
    └── Namespace "cache"
        └── CacheEntryResource
```

- **Application** — your code. The library never imposes an
  architecture; you wire the manager in however you like.
- **`TempManager`** — the root object. Created with
  `TempManager::builder()`, it canonicalizes one root directory, opens
  (or creates) the instance directory `root/<instance_id>` inside it,
  and holds the shared registry, backends, lease book and statistics
  for everything below it.
- **`Namespace`** — a logical isolation boundary inside a manager.
  Every resource belongs to exactly one namespace. Namespaces carry
  their own quota, metadata and cleanup policy. Namespaces nest:
  `manager.namespace("parent/sub")` creates `sub` below `parent` (see
  [namespaces.md](namespaces.md)).
- **Resource** — one tracked temporary object: a file, a directory, a
  buffer, a cache entry, a serialization intermediate, a runtime
  marker, or a custom type plugged in through a backend.

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-model-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;

    let scratch = manager.namespace("scratch")?;   // logical boundary
    let file = scratch.create_file("packet.bin")?;  // a resource in it

    println!("{} -> {}", file.id(), file.path().display());

    scratch.release(file.id())?;                    // reclaim it
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

## The registry and resource metadata

One registry per manager holds every live resource as a `ResourceMeta`
record. The registry is the crate's bookkeeping backbone: quotas,
cleanup, statistics and recovery all read from it. The important
fields:

| field             | what it means                                                        |
| ----------------- | -------------------------------------------------------------------- |
| `id`              | stable, unique `ResourceId`; survives registry re-registration       |
| `kind`            | `File` / `Directory` / `Buffer` / `Cache` / `Serialization` / `Runtime` / `Custom` |
| `namespace`       | the namespace the resource belongs to (full path, e.g. `a/b`)        |
| `state`           | `ResourceState`; see [lifecycle-management.md](lifecycle-management.md) |
| `size_bytes`      | last recorded size; `0` for files until `refresh_size` syncs it      |
| `created_at`      | creation time; drives `Ttl::Fixed`                                   |
| `last_accessed`   | last read/access time; drives `Ttl::Idle`                            |
| `ttl`             | `Option<Ttl>` — fixed-from-creation or idle-from-access              |
| `owner`           | free-form component/request tag from `CreateOptions::owner`          |
| `priority`        | eviction priority; falls back to the effective policy priority       |
| `metadata`        | free-form string map from `CreateOptions::metadata`                  |
| `cleanup_attempts`| how many times cleanup tried and failed to destroy the resource      |

Records are inserted at creation time and removed when the resource is
destroyed. Creation uses a two-phase protocol inside the registry: an
`Uninitialized` placeholder is reserved first (while manager-wide and
ancestor quotas are checked against atomic subtree totals), and the
backend materializes the physical resource outside the registry lock;
publishing then moves the record to `Created`. If the backend fails,
the reservation rolls back — a failed creation never leaves a registry
record behind.

Sizes are accounted, not measured: the recorded `size_bytes` is what
quotas and reports use. Files register with `0` until
`namespace.refresh_size(id)` syncs from `fs::metadata`.

## Ownership modes

Every resource is created with one of four ownership modes. The mode
decides who destroys the resource and when — see
[ownership-and-leases.md](ownership-and-leases.md) for the full guide.

| mode      | last handle drop destroys? | `release` destroys? | cleanup destroys? | default |
| --------- | -------------------------- | ------------------- | ----------------- | ------- |
| `Owned`   | yes                        | yes                 | yes               |         |
| `Shared`  | yes (the last co-owner)    | yes                 | yes               |         |
| `Managed` | no — handle detaches       | yes                 | yes               | yes     |
| `Leased`  | no — handle detaches       | yes, unless leased  | yes, unless leased |        |

"Destroy" always means the same thing: the record is claimed from the
registry, the owning backend's `release(meta)` is called, and counters
and events are bumped. Nothing else ever deletes a file.

## Leases

A lease is an RAII guard on a single resource. `handle.acquire()`
increments the resource's lease count in the manager-wide lease book;
dropping the lease decrements it. While at least one lease is held, no
destruction path may physically remove the resource — not an explicit
`release`, not a cleanup pass, not an owning drop. The attempt
instead moves the record to `PendingCleanup` and reports a deferral.

Leases are the crate's only notion of "this operation is still in
flight". They are also what `shutdown_wait_leases` waits on, and what
`active_leases` in the statistics snapshot counts. Dropping the last
lease does **not** itself destroy a `PendingCleanup` resource — the
next release call or cleanup pass does. Leases are a gate, not a
trigger.

## Lifecycle states

Every record carries a `ResourceState`. The happy path is
`Uninitialized → Created → (Active) → Expired → PendingCleanup →
Destroyed`; lease-blocked destruction and backend failures detour
through `PendingCleanup` and `FailedCleanup` respectively. `Locked`,
`Invalid` and `Recovering` exist for operation-scoped and reserved
flows. The full state machine — who transitions what, catch-all
behaviour, `is_cleanup_eligible`, and what `CleanupReport` shows — has
its own page: [lifecycle-management.md](lifecycle-management.md).

## Backends

Three backends exist:

- **filesystem** (default) — real files and directories under
  `root/<instance>/<namespace>/<name>`, created with `create_new(true)`,
  confined to the canonicalized root, with symlink policies and
  startup crash recovery.
- **memory** — accounting-only backend for buffers, serializations
  and runtime markers; byte limits are enforced at creation.
- **custom** — your own storage, via `ResourceBackend` and
  `TempManager::register_backend` (see
  [writing-a-backend.md](writing-a-backend.md)).

A resource is destroyed by the backend that created it, recorded in
its metadata. Built-in resources never bypass their backend.

## No background threads, no scheduler, no runtime

The library spawns nothing. There is no garbage-collecting thread, no
built-in timer, no requirement for Tokio or any other executor, and no
hidden work between API calls. Reclamation happens when you call it:

- `manager.cleanup_expired()` — one pass over the whole manager;
- `namespace.cleanup_expired()` — one pass over the namespace and its
  registered descendants;
- `namespace.release(id)` — destroy one resource now;
- `manager.shutdown()` — optional sweep plus lock-file removal (and
  optionally instance-directory removal);
- ordinary owning drops for `Owned`/`Shared` resources.

If you want periodic cleanup, run it from your own scheduler; see
[scheduler-integration.md](scheduler-integration.md). If you want an
async API, the `async`/`tokio` features wrap the same sync core; see
[sync-and-async.md](sync-and-async.md).

## Multiple instances on one root

Several managers may share a root directory (several processes, or
several managers in one process). Each instance gets its own directory
`root/<instance_id>` and its own lock file; recovery uses those to
teer stale data from crashed runs without touching live instances. A
manager never adopts another instance's directory, and shutdown removes
only its own lock file (or its own instance directory, if
`remove_instance_dir_on_shutdown` is enabled — off by default).

## What the library never does

- No background threads, timers, or auto-schedulers.
- No global state; independent managers coexist.
- No forced filesystem layout — the root is yours to choose.
- No forced runtime; the sync API always works.
- No silent name "sanitizing" — invalid names are rejected with typed
  errors.
- No implicit permission changes — files and directories are created
  with the process's umask.
- No cross-namespace operations — per-resource isolation is absolute,
  in every API, on every path.
- No reading of file *contents* — the manager tracks sizes and
  metadata, not bytes.
