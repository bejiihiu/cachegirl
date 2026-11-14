# Error handling

The crate has one top-level error enum, `cachegirl::Error`, wrapping
nine typed category enums. Matching on the category tells you exactly
what kind of failure happened; the `#[from]` conversions mean `?` does
the wrapping for you.

## The categories

```text
Error::Create(ResourceCreationError)
Error::Backend(BackendError)
Error::Quota(QuotaError)
Error::Access(AccessError)
Error::Registry(RegistryError)
Error::Cleanup(CleanupError)
Error::Recovery(RecoveryError)
Error::Concurrency(ConcurrencyError)
Error::Config(ConfigError)
```

`Error` is `#[non_exhaustive]`; new categories may be added without a
major version bump, so downstream `match` must have a catch-all arm.

### ResourceCreationError

Creation-time failures: `InvalidName(String)` (name failed validation —
typo class), `UnsupportedKind { backend, kind }` (backend does not
support the requested kind), `Backend { backend, source }` (the backend
failed; `source()` is the wrapped `BackendError`), `Io(#[from] std::io::Error)`.

### BackendError

Failures reported by storage backends: `Unavailable { backend, reason }`
(no such backend registered, or backend unusable),
`Io { backend, source }` (I/O inside a backend),
`Rejected { backend, reason }` (policy refusal — e.g. file already
exists, blob-store refused an unsafe name, cache key taken),
`PathEscape(PathBuf)` (a path would leave the backend root).

### QuotaError

`SizeExceeded { limit, requested }` — the byte quota would be exceeded
(`requested` is the projected usage, saturated at `u64::MAX`);
`CountExceeded { limit }` — the count quota would be exceeded;
`Namespace { namespace, reason }` — namespace-specific refusal (used by
the namespace quota check; the built-in path reports `SizeExceeded` /
`CountExceeded` today, the variant exists for backends and future
limits).

### AccessError

Access and safety violations: `PermissionDenied(String)` (OS refused),
`OutsideRoot { path, root }` (path escapes the canonical root),
`SymlinkRejected(PathBuf)` (symlink refused by policy),
`UnsafeName(String)` (name failed validation).

### RegistryError

Bookkeeping: `NotFound(ResourceId)` (no such record, or — from a
namespace's point of view — a record belonging to another namespace),
`AlreadyRegistered(ResourceId)` (double registration),
`InvalidTransition { from, to }` (lifecycle state machine refused the
move), `Poisoned` (registry mutex was poisoned by a panic).

### CleanupError

Cleanup-time failures: `Removal { id, reason }` (a resource could not
be removed), `Io(#[from] std::io::Error)`, `Leased(ResourceId)` (the
resource is still leased). Note that lease deferral is not an error:
it counts as `deferred` in the report. `CleanupError::Leased` is
reserved for APIs that are required to fail rather than defer.

### RecoveryError

Startup recovery: `Inspect { path, source }` (cannot examine a
directory), `CorruptLock { path, reason }` (lock unparsable — recovery
never deletes on a corrupt lock), `LiveInstance { path, pid }` (would
remove a live process's data — unreachable through the public recovery
path; present for the internal invariant).

### ConcurrencyError

Concurrency and shutdown: `Locked(ResourceId)` (whoever holds the
try-lock), `LeaseLimit(ResourceId)` (per-resource lease limit),
`Destroying(ResourceId)` (destruction in flight; new leases refused),
`Cancelled` (the operation was cancelled), `BlockingTaskPanic(String)`
(a blocking task on the async facade panicked), `ManagerClosed` (manager
already shut down), `Poisoned` (a synchronization primitive was
poisoned).

### ConfigError

Configuration: `RootMissing(PathBuf)` (root does not exist and
`create_if_missing(false)`), `RootCreate { path, source }` (cannot
create root or instance directory), `InvalidInstanceId(String)` /
`InvalidNamespaceName(String)` (name validation), `Invalid(String)`
(everything else — e.g. `ttl` + `idle_ttl` together, `max_size(0)`).

## Display and source()

Every variant renders a non-empty, single-line message through
`Display` (unit-tested for all of them). The top-level `Error` is
`#[error(transparent)]` over each category: the wrapper adds nothing of
its own, so `err.to_string()` on `Error::Quota(QuotaError::CountExceeded { limit })`
contains `count quota exceeded: limit is N resources`, not
`quota: count quota...`.

Categories with a `#[source]` expose `source()` as the underlying
cause: `ResourceCreationError::Backend::source()` → `BackendError`;
`Io(#[from] std::io::Error)` → the `std::io::Error`. Walk the chain
with `std::error::Error::source()` when you need the root cause for
logs. The outer `Error` does not declare `source()` of its own — it is
transparent, so `source()` returns the inner category error, whose own
`source()` returns the OS error.

Errors do not implement `Serialize` (the `std::io::Error` payload
cannot); persist `Statistics`, `ResourceMeta` or reports instead.

## Retrying cleanup: MAX_CLEANUP_ATTEMPTS

A resource whose physical removal fails lands in state `FailedCleanup`
and is retried by later passes. After `MAX_CLEANUP_ATTEMPTS` (3)
attempts the pass stops touching it — the record survives for
inspection and is skipped silently from then on. Two consequences:

- A permanently failing backend accumulates "stuck" records instead of
  looping or crashing; nothing in a cleanup pass is allowed to take
  the process down.
- If your pass history shows records stuck in `FailedCleanup` with
  `cleanup_attempts >= 3`, the fix is out-of-band: repair the cause
  (permissions, locked file, full disk), release the id manually
  (`ns.release(&id)` retries destruction), or remove the file by hand
  and restart.

Cleanup failures never abort the pass and are never returned as the
pass's `Result::Err` — they are collected into
`CleanupReport::failure_reasons` (one string per failure) and counted
in `failed`. `Result::Err` from `cleanup_expired()` means the *pass
itself* could not run (poisoned registry, unreadable root), not that
one resource was stubborn.

## Poisoned registries

The registry and the lease book are plain `Mutex`es. If a panic happens
while one is held, subsequent operations return
`ConcurrencyError::Poisoned` / `RegistryError::Poisoned` instead of
observing half-updated state. In practice this means:

- a panic inside a create/destroy path leaves the manager consistent,
  not wedged — except the poisoned mutex refuses all further
  operations on the poisoned structure;
- recovering means restarting the process (or the manager); there is
  no `unpoison` API and there should not be — the invariants that
  protect you (lease gate, single destruction path) were written
  assuming a working lock.

Event sinks are called under no lock but their own; a panicking sink
is caught at the destruction path (`OwnershipCore` uses
`catch_unwind`) so a sink bug cannot poison the registry — but a
panicking sink elsewhere aborts the operation it was attached to, so
keep sinks panic-free.

## Recommended handling strategy

```rust
use cachegirl::{Error, QuotaError, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-errors-{}", std::process::id()));
    let manager = match TempManager::builder().root(&dir).build() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("startup failed: {e}");
            return Err(e);
        }
    };

    match manager.namespace("api") {
        Ok(ns) => match ns.create_file("scratch.bin") {
            Ok(file) => println!("created {}", file.id()),
            Err(Error::Quota(QuotaError::CountExceeded { limit })) => {
                // expected backpressure: shed load or grow capacity
                eprintln!("at the count quota ({limit}); refusing new scratch file");
            }
            Err(e) => eprintln!("unexpected creation failure: {e}"),
        },
        Err(e) => eprintln!("namespace failure: {e}"),
    }

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Rules of thumb:

- Match on the `Error::` category for control flow; print the message
  for humans; carry `source()` for logs when something unexpected
  happens.
- Treat `Quota` and `Locked`/`Destroying` as expected control flow in
  hot paths; treat everything else as a bug or an environment problem.
- Check `CleanupReport::failed > 0` on the shutdown path and log the
  reasons; do not retry inline.
- In async code, `BlockingTaskPanic` means a backend panicked; treat
  it as fatal for that operation and inspect your backend, do not
  loop-retry.
