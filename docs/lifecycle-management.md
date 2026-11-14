# Lifecycle management

Every resource the registry tracks carries a `ResourceState`. The
state machine is the crate's bookkeeping backbone: illegal moves are
rejected with `RegistryError::InvalidTransition` instead of silently
corrupting accounting. This page is the full guide to the states, who
moves a record between them, and what the cleanup engine does with
each state.

Most applications never touch `ResourceState` directly. It shows up in
`ResourceMeta::state`, in statistics (`expired`, `pending_cleanup`),
and in the `CleanupReport` breakdown. Read this when you are building
a janitor, an operator dashboard, or a backend, or when a resource is
stuck in a state you did not expect.

## The states

| state            | meaning                                                     | terminal | cleanup-eligible |
| ---------------- | ----------------------------------------------------------- | -------- | ---------------- |
| `Uninitialized`  | reservation placeholder inserted while quotas are checked and the backend materializes the resource; never visible in the API | no       | no               |
| `Created`        | created and registered, not (yet) in active use             | no       | no               |
| `Active`         | in active use by the application                            | no       | no               |
| `Locked`         | temporarily locked by an exclusive operation                 | no       | no               |
| `Expired`        | outlived its TTL or nominated by eviction; eligible         | no       | **yes**          |
| `PendingCleanup` | destruction requested, but leases held; awaiting the last lease | no       | **yes**          |
| `Destroyed`      | reclaimed                                                   | **yes**  | no               |
| `FailedCleanup`  | physical removal failed; kept for inspection or retry       | no       | no               |
| `Invalid`        | marked invalid, e.g. after an integrity check failed        | no       | no               |
| `Recovering`     | being rebuilt after a crash or interrupted operation        | no       | no               |

`is_cleanup_eligible()` is true exactly for `Expired` and
`PendingCleanup`. `is_terminal()` is true exactly for `Destroyed`.
Self-transitions are never legal.

Reading the state from a live record:

```rust
use cachegirl::{ResourceState, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-state-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("demo")?;
    let file = ns.create_file("state.bin")?;

    let meta = manager.registry().get(file.id())?;
    println!("state: {:?}", meta.state);
    println!("cleanup-eligible: {}", meta.state.is_cleanup_eligible());
    println!("terminal:         {}", meta.state.is_terminal());
    println!(
        "Created -> Active is legal: {}",
        ResourceState::Created.can_transition_to(ResourceState::Active)
    );

    ns.release(file.id())?;
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

An honest note on coverage: `Created`, `Active` and the `Expired →
PendingCleanup → (removal)` path are the states the crate moves
records through today. `Uninitialized` is used internally by the
creation reservation. `Destroyed`, `Locked`, `Invalid` and `Recovering`
are part of the state machine and the transition table, but no public
API transitions into them yet — `Destroyed` in particular is never
written to a surviving record; a resource that is reclaimed is
*removed* from the registry, not marked. They exist so the machine is
complete, not because the crate secretly transitions through them.

## The legal transitions

The whole table, exactly as enforced by `ResourceState::can_transition_to`:

| from             | to                                              |
| ---------------- | ----------------------------------------------- |
| `Uninitialized`  | `Created`, `Invalid`                            |
| `Created`        | `Active`, `Locked`, `Expired`, `PendingCleanup`, `Invalid`, `Recovering` |
| `Active`         | `Locked`, `Expired`, `PendingCleanup`, `Invalid`, `Recovering` |
| `Locked`         | `Active`, `Expired`, `PendingCleanup`, `Invalid`, `Recovering` |
| `Expired`        | `PendingCleanup`                                |
| `PendingCleanup` | `Destroyed`, `FailedCleanup`                    |
| `FailedCleanup`  | `PendingCleanup`, `Invalid`                     |
| `Invalid`        | `PendingCleanup`                                |
| `Recovering`     | `Active`, `FailedCleanup`, `Invalid`            |
| `Destroyed`      | — (no legal moves; terminal)                    |

Everything else is rejected. Self-transitions are rejected too — the
table lists real moves only.

## Who performs the transitions

| transition                            | driven by                                                       |
| ------------------------------------- | --------------------------------------------------------------- |
| insert as `Uninitialized`             | creation: registry reservation before the backend runs          |
| `Uninitialized → Created`             | creation: publish step after the backend materialized the resource |
| `Uninitialized → Invalid`             | creation: reserved for creation failure handling (table-legal)   |
| `Created → Expired`                   | cleanup pass: TTL lapsed or eviction nominated the resource      |
| `Active → Expired`                    | cleanup pass: as above                                          |
| `Locked → Expired`                    | cleanup pass: as above (locked resources may still expire)       |
| `Created/Active/Locked → PendingCleanup` | a destruction path while leases are held (`release`, cleanup, owning drop) |
| `Expired → PendingCleanup`            | cleanup pass, immediately before physical destruction            |
| `PendingCleanup → Destroyed`          | logically the destroy step; the record is removed from the registry |
| `PendingCleanup → FailedCleanup`      | backend refused the physical removal                             |
| `FailedCleanup → PendingCleanup`      | retry by a later cleanup pass (`cleanup_attempts < MAX_CLEANUP_ATTEMPTS`) |

The destroy path is shared by every caller: `namespace.release(id)`,
owning drops for `Owned`/`Shared` resources, and cleanup passes all
funnel through one `destroy_resource`. It first seals the id in the
lease book; on success the record is claimed from the registry with an
atomic `remove` (a concurrent release loses the race and observes
`AlreadyGone`, not a double-free); the backend releases the physical
resource; counters and events fire. On failure the record is
re-registered in `FailedCleanup` with `cleanup_attempts` bumped, so a
later pass can retry.

## The creation path in detail

1. `namespace.create_*` validates names, resolves quotas (manager-wide
   plus every ancestor namespace, atomically against registry subtree
   totals), and reserves an `Uninitialized` placeholder in the registry.
2. The backend materializes the resource (a file on disk, a buffer
   allocation, a cache entry) — outside the registry lock.
3. On success the reservation is published: `Uninitialized → Created`,
   with the real size recorded.
4. On failure the reservation rolls back; no record, no half-created
   resource visible to the application.

## The cleanup path in detail

A cleanup pass (`manager.cleanup_expired()`, `namespace.cleanup_expired()`,
or the shutdown sweep) has three phases, documented in
[cleanup-policies.md](cleanup-policies.md). State-wise, what matters:

- **TTL-expired** `Created`/`Active`/`Locked` records are moved to
  `Expired` (needs a transition first) and then destroyed.
- Already-`Expired` and `PendingCleanup` records skip the TTL check
  and go straight to the destroy step — they are `eligible`.
- **`FailedCleanup`** records are retried while
  `cleanup_attempts < MAX_CLEANUP_ATTEMPTS` (3); past that they are left
  alone for a human to inspect.
- **Quota eviction** only nominates `Created`/`Active`/`Locked`
  victims; the victim moves to `Expired` and is destroyed in the same
  pass.
- **`Locked` resources are not exempt** from expiry or eviction; the
  lock is a bookkeeping hint, not a lease. If a resource must not be
  touched, it needs a lease, not `Locked`.

## Leases gate destruction, not states

`PendingCleanup` is the only state a lease can force. Whenever a
destruction path finds at least one active lease on the resource, it
moves the record to `PendingCleanup` and reports a deferral instead of
destroying. The lease book (a manager-wide per-id counter) is checked
*before* the registry record is claimed, under an atomic seal: while
the seal is in place, `handle.acquire()` fails with
`ConcurrencyError::Destroying` rather than handing out a lease that
would outlive the destruction. Dropping the last lease does not
resurrect or trigger destruction — a later release or cleanup pass
finishes the job.

## How a custom cleanup policy affects the machine

`CustomCleanupPolicy::decide(meta, ctx)` returns `Keep` or `Remove`
for resources that are not otherwise eligible. A `Remove` verdict is
treated exactly like a TTL lapse: `Created`/`Active`/`Locked` →
`Expired` → destroy. If the resource is leased at that moment, the
destroy step defers to `PendingCleanup` as usual — a custom policy
cannot override a lease any more than a TTL can. `CustomCleanupPolicy`
is consulted only in the expiry-sweep phase; it does not participate in
quota eviction.

## What a CleanupReport shows

Every pass returns a `CleanupReport`; the lifecycle-relevant fields:

- `scanned` — records examined (in-scope only);
- `removed` — records destroyed and unregistered;
- `skipped` — records observed but not acted on;
- `failed` — destruction attempted but refused; the record sits in
  `FailedCleanup` and `failure_reasons` carries one string per failure;
- `deferred` — destruction attempts held back by leases; the records
  sit in `PendingCleanup`;
- `bytes_freed` — sum of destroyed records' last recorded sizes;
- `reasons` — per-cause breakdown (`RemovalReasons`): `ttl_fixed`,
  `ttl_idle`, `size_eviction`, `count_eviction`, `policy`,
  `lease_deferred`, `eligible`;
- `duration` — wall-clock of the pass.

`RemovalReasons::destroyed()` sums every removal cause *excluding*
`lease_deferred`; it saturates at `u64::MAX`. `CleanupReport::is_quiet()`
ignores skips and deferrals (true when nothing was removed and nothing
failed); `RemovalReasons::is_quiet()` additionally counts deferrals.

## Statistics view

`Statistics::snapshot()` splits live records into `expired` (records in
`Expired`) and `pending_cleanup` (records in `PendingCleanup`) alongside
the totals, so a health check can tell "TTL is lapsing normally" from
"leases are piling up and nothing is being reclaimed" without reading
records.

## Observing a stuck resource

- `PendingCleanup` piling up → leases are leaking or genuinely long-held;
  check who holds `Lease`s. `shutdown_wait_leases` exists for exactly
  this at shutdown time.
- `FailedCleanup` piling up → the backend cannot remove something;
  check `failure_reasons`, permissions, locked files. After 3 attempts
  the record is parked; fix the cause, then `release(&id)` again.
- `resources` never going down → cleanup is not running; call
  `cleanup_expired` from your scheduler (see
  [scheduler-integration.md](scheduler-integration.md)).
