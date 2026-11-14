# Writing a backend

A backend is the physical side of a resource. The manager never
hard-codes one; it talks to a three-method contract, which is what
keeps the core usable without a filesystem and what lets you plug
anything in: a blob store, a database, a remote object cache, an
in-memory map for tests.

## The contract

```text
pub trait ResourceBackend: Send + Sync {
    fn name(&self) -> &str;
    fn create(&self, request: &ResourceRequest) -> Result<ResourceMeta>;
    fn release(&self, meta: &ResourceMeta) -> Result<()>;
}
```

(`ResourceBackend` and `ResourceRequest` live in
`cachegirl::backend`.)

### `name`

A stable, human-meaningful identifier (`"filesystem"`, `"blob-store"`,
...). It is used in error messages, metrics labels and registry
metadata (`meta.metadata["backend"]`). Re-registering a backend under
an existing name replaces it. Do not reuse a built-in name by accident.

### `create`

Materialize the resource described by `ResourceRequest` and return a
`ResourceMeta`. `ResourceRequest` carries `name` (validated — it will
never contain `/`, `\`, `:`, control characters, trailing dots or
reserved device names), `namespace` (same validation), `kind`,
an optional `size_hint` (bytes, if known — you may enforce it), and
free-form `metadata`.

Requirements:

- **Confine all physical operations to your own root.** The caller is
  not required to trust you; you are required to confine yourself.
  Reject names and resolved paths that would escape.
- **Never clobber.** If the name is taken, fail. Losing an existing
  file is the one unrecoverable error in this contract.
- **Never report a `ResourceMeta` you did not create.** The manager
  registers whatever you return, and cleanup will later call
  `release` on it. Return a meta whose `metadata` carries everything
  `release` needs to find and destroy the physical object (a path, a
  key, a handle — anything opaque to the manager).
- Fill `size_bytes` with the best known size (0 is acceptable for a
  resource that grows later); `owner`, `created_at` and friends are
  set by the manager or by you — what matters for the registry is a
  consistent meta.
- It is fine to return `UnsupportedKind` for kinds you do not handle —
  that is how the built-in filesystem backend refuses `Buffer`.

### `release`

Destroy the resource described by `meta`.

Requirements:

- **Idempotent.** Cleanup passes may see the same record more than
  once (a race loser observes `AlreadyGone` upstream of you, but your
  backend may still be called after a partial failure). Releasing
  something already gone must succeed silently.
- **Complete confinement.** `release` receives the meta as-is, which
  may originate from a file you did not create (a crashed run's
  record, a tampered one). Never act on a path outside your root;
  refuse it instead. A `release` that deletes outside your root is
  the one bug this contract exists to prevent.
- **Fast and fallible.** Release is called inline from destructors,
  cleanup passes and explicit `release` calls. Errors are reported
  through the report/events; do not panic.

## Object safety and registration

The trait is object-safe on purpose so backends can be registered and
swapped at runtime. Register one with `manager.register_backend(...)`:

```rust
use std::sync::Arc;
use cachegirl::{
    backend::{ResourceBackend, ResourceRequest},
    CreateOptions, TempManager,
};

struct NullBackend;

impl ResourceBackend for NullBackend {
    fn name(&self) -> &str {
        "null-backend"
    }
    fn create(&self, request: &ResourceRequest) -> cachegirl::Result<cachegirl::ResourceMeta> {
        Ok(cachegirl::ResourceMeta::new(
            &request.name,
            &request.namespace,
            request.kind,
        ))
    }
    fn release(&self, _meta: &cachegirl::ResourceMeta) -> cachegirl::Result<()> {
        Ok(())
    }
}

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-backend-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    manager.register_backend(Arc::new(NullBackend))?;

    let ns = manager.namespace("demo")?;
    let res = ns.create_custom_with(
        "thing",
        "null-backend",
        CreateOptions::new().owner("example"),
    )?;
    println!("created via backend: {}", res.backend());
    println!("release -> {}", ns.release(res.id())?);

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

An unknown backend name fails at creation with
`BackendError::Unavailable { backend, reason }` — not during registry
registration.

## A complete example: a blob store

The `custom_backend.rs` example ships a full, working backend: every
resource is an opaque blob file written under the backend's own root,
with an in-memory op log for observability. Its shape is the one to
copy:

1. Confine to a canonicalized root of its own.
2. Reject unsafe `namespace`/`name` values on the far side of the
   trust boundary — the manager validates, but a second, local check
   is mandatory (defense in depth, and it covers records that
   originated elsewhere).
3. `create_new(true)` semantics: never overwrite an existing blob.
4. `release` re-derives the path from `meta.metadata["path"]`, refuses
   anything outside the confined root, deletes, and treats "already
   gone" as success.
5. At-creation `size_bytes` is 0 (the blob is empty then); the example
   notes that `bytes_freed` bookkeeping reports what creation claimed,
   not the on-disk size the backend reclaims.

Registry bookkeeping always reflects what the backend reported at
creation. If your backend can learn the real size at release time, the
manager does **not** get a chance to fix `bytes_freed` retroactively —
it counts the destruction once, from the record it holds.

## What the manager agrees to

- Resources created through your backend are registered with
  `ResourceKind::Custom`, routed back to your backend on `release`
  (the registry record carries `backend` in its metadata; kinds need
  no mapping), and appear in quotas, reports, events and statistics
  like any built-in resource.
- `create` failures roll the physical side back best-effort and leave
  nothing in the registry (a failed creation never leaks a record).
- `release` failures re-register the record in `FailedCleanup` with
  one attempt counted; later cleanup passes retry within
  `MAX_CLEANUP_ATTEMPTS` (3), then leave the record for inspection.
- `metadata` you return from `create` is free text as far as the
  manager cares; your `release` may rely on it completely.

## What the manager does not require

- It does not require a filesystem. A pure in-memory or remote backend
  is fine; `docs` here assume nothing about storage.
- It does not require `Clone`, `Debug` or any serialization of your
  backend.
- It does not spawn you work you did not ask for: no background
  janitor calls your backend except from explicit cleanup passes and
  explicit release calls.

## Testing your backend

The pattern the integration suite uses: a logging backend wrapped
around a real or fake store, with counters for `create`/`release`
calls and payloads captured for assertion. Register it, drive a full
lifecycle (create → write through the path/handle → release), assert
the op counts and the registry state afterwards. Failure injection is
a first-class citizen here: a backend whose `create` fails must leave
the registry empty, and a backend whose `release` fails must leave the
record in `FailedCleanup` with the attempt counted — those are the
two invariants the manager depends on.

## Quotas and accounting caveat

Cleanup counts bytes from the **registry record**, not from your
backend. If your backend stores variable-size payloads that grow
after creation, either accept that `bytes_freed` and quota eviction
under-report, or grow the record's `size_bytes` yourself through the
public registry API — `manager.registry().update(&id, |meta|
meta.size_bytes = new_size)` — at the points where you learn the new
size. The manager asks your backend for nothing during cleanup, by
design; the registry is the single source of truth for accounting.
