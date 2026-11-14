# Ownership and leases

Every resource is created with one of four ownership modes. The mode
decides what happens to the physical resource when handles drop; the
lease system decides whether a deletion may happen at all right now.
The two mechanisms are orthogonal and both must let a deletion through.

## The four ownership modes

| mode      | last handle drop destroys? | explicit `release` works? | cleanup pass may destroy? | default |
| --------- | -------------------------- | ------------------------- | ------------------------- | ------- |
| `Owned`   | yes — via the backend      | yes                       | yes (or defers)           |         |
| `Shared`  | yes — the last co-owner    | yes                       | yes (or defers)           |         |
| `Managed` | no — handle detaches       | yes                       | yes (or defers)           | yes     |
| `Leased`  | no — handle detaches       | yes (or defers)           | only when idle, i.e. defers while leased |         |

"Destroy" here always means: the record is claimed from the registry,
the owning backend's `release(meta)` is called, counters and events are
bumped. Nothing else ever deletes a file.

### Owned

Single-owner semantics. `drop` of the last owning handle destroys the
resource. If leases are still held at that moment, destruction is
deferred — the record moves to `PendingCleanup` and a later cleanup
pass (or an explicit release after the leases drop) finishes the job.
The file never leaks silently; it becomes visible as `pending_cleanup`
in the statistics snapshot.

### Shared

Arc-like co-ownership. Every clone of the handle is a co-owner; the
resource is destroyed when the last one drops. This is the mode for
"two components both hold the same scratch file, neither knows exactly
when the other is done". The underlying `Arc<OwnershipCore>` makes
"last drop" race-free; there is no explicit `strong_count` check
anywhere.

### Managed

The default. Dropping the handle detaches it; the resource lives until
it is released explicitly, expires under its TTL, or a cleanup pass
reclaims it. Use `Managed` when the filesystem (or the backend) is the
source of truth and the handle is just a view — e.g. a file you will
later verify by other means.

### Leased

Lease-centric ownership. Dropping the handle detaches it. The resource
is only ever reclaimed through explicit release or a cleanup pass, and
both are refused while leases are held — the resource moves to
`PendingCleanup` instead. `Leased` exists for resources whose physical
removal must never happen out from under a reader, regardless of what
the policy layer decides.

## Leases: the gate on deletion

A lease is an RAII guard. `handle.acquire()` increments a counter in the
manager-wide `LeaseBook`; dropping the lease decrements it. While at
least one lease is held on a resource, *no* destruction path may remove
it — not explicit release, not a cleanup pass, not an owning drop.
Instead, the record is moved to `PendingCleanup` and the operation
reports the deferral:

- `Namespace::release` returns `Ok(false)` (and leaves the record in
  `PendingCleanup`),
- a cleanup pass counts the resource in `deferred` and in
  `reasons.lease_deferred`,
- `Owned`/`Shared` drops leave the file on disk with the record marked
  `PendingCleanup`.

Once the last lease is gone, the next deletion attempt succeeds. Note
that dropping the last lease does **not** itself destroy a
`PendingCleanup` resource — reclamation happens on the next release
call or cleanup pass. Leases are a gate, not a trigger.

```rust
use cachegirl::{CreateOptions, Ownership, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-lease-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let file = ns.create_file_with(
        "leased.bin",
        CreateOptions::new().ownership(Ownership::Leased),
    )?;
    let path = file.path().to_path_buf();
    let id = file.id().clone();

    let lease = file.acquire()?;
    println!("release with lease held: {}", ns.release(&id)?);
    assert!(path.exists(), "a held lease must block deletion");

    drop(lease);
    println!("release after lease:     {}", ns.release(&id)?);
    assert!(!path.exists());

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Leases are counted manager-wide, are `Send` and can move to other
threads, and are counted in `Statistics::active_leases`. They survive
unwinding: a panicking holder releases its lease during unwind, so
counts never leak upward.

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-stats-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;

    let file = ns.create_file("hot.bin")?;
    let lease_a = file.acquire()?;
    let lease_b = file.acquire()?;
    println!("active leases: {}", manager.stats()?.active_leases);
    drop(lease_a);
    drop(lease_b);
    println!("active leases: {}", manager.stats()?.active_leases);

    ns.release(file.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

## The destruction race, and how it is closed

Both the cleanup engine and release go through one path: seal the id in
the lease book (fails atomically while any lease is held), then claim
the record from the registry with `remove`. Whoever loses the race
observes `AlreadyGone` instead of double-releasing. There is no window
where a resource is half-destroyed or double-counted; the seal covers
every exit path including panics from a misbehaving event sink.

## PendingCleanup, and what "deferred" means in reports

`PendingCleanup` is a normal, visible state — not an error. The report
surfaces it as `deferred` and `reasons.lease_deferred`, and
`Statistics` surfaces it as `pending_cleanup`. If your sweep shows a
permanently growing `pending_cleanup`, something is acquiring leases and
not dropping them — usually an early `return` or `?` in a long-lived
worker. The fix is a scope guard or `defer(lease.drop())`-style pattern,
not a bigger cleanup budget.

## The lease-destroy race

`acquire()` refuses a resource whose destruction is in flight: it fails
with `Error::Concurrency(ConcurrencyError::Destroying(id))`. This is
the dual of the seal — destruction seals first, so a lease can never
slip in after the physical removal started.

## Panic safety

- Dropping `Owned`/`Shared` handles destroys through a
  `catch_unwind`-guarded path: a panicking event sink cannot take the
  process down, and the destruction either completes or is recorded as
  `FailedCleanup` for a later pass to retry.
- A panicking lease holder releases its lease during unwind — the
  counter never leaks.
- A panic between sealing and destruction unwinds through a guard that
  unseals, so the id does not get stuck refusing new leases.
- The registry's internal lock poisons on panic; operations after that
  return `Error::Concurrency(ConcurrencyError::Poisoned)` instead of
  corrupting state. See [error-handling.md](error-handling.md).

## Drop and async contexts

`Drop` is synchronous, and it does I/O for `Owned`/`Shared` resources
(filesystem removal goes through the backend). Dropping an owning
handle on an async executor therefore does blocking work inline — that
is allowed, but it stalls the executor thread for the duration of the
removal. Two recommendations:

- drop owning handles inside `tokio::task::spawn_blocking` (or any
  blocking pool), or
- make the resource `Managed` and release it explicitly with an await
  (`ns.release(&id).await` in the async facade), which offloads the
  same work deliberately.

`release` itself never runs inside a destructor, so this only concerns
drops of owning handles. The async facade never wraps handles in async
traits for exactly this reason.

## LockSet and LockGuard

`LockSet`/`LockGuard` are a non-blocking try-lock for exclusive
operations on a resource — "one of us mutates the resource, the others
fail fast with `ConcurrencyError::Locked`". They are **advisory**: the
lease book is what actually gates destruction. Locks exist so
application-level critical sections fail fast instead of queueing.
`LockSet` itself is not consulted by cleanup or release.

## Ownership quick reference

```text
             Owned   Shared   Managed  Leased
drop last    destroy destroy  detach   detach
release      destroy  destroy  destroy  destroy*
sweep (idle) destroy  destroy  destroy  destroy*
*unless leases are held, in which case: PendingCleanup
```
