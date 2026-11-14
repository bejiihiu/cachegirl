# Cleanup policies

`cachegirl` never reclaims anything by itself — there is no background
thread. A cleanup pass happens when you call `manager.cleanup_expired()`
or `namespace.cleanup_expired()` (typically on a scheduler or after a
request). This page explains what the pass does, in what order, what
the knobs are, and what the report tells you afterwards.

## The pass in three phases

1. **Expiry sweep.** Every record in scope is examined. Effective TTL
   resolution order: the record's own `ttl` (set at creation via
   `CreateOptions` or baked in from `default_ttl`), otherwise the
   effective policy of the namespace, otherwise the manager-wide
   policy. If the TTL has lapsed (fixed from creation, or idle from
   last access), the record moves to `Expired` and is destroyed. The
   same phase also consults a custom cleanup policy for resources not
   otherwise eligible — a `Remove` verdict destroys in the same pass.
   Records already in `Expired` or `PendingCleanup` are picked up
   directly (`eligible`), and records in `FailedCleanup` are retried
   while `cleanup_attempts < MAX_CLEANUP_ATTEMPTS`.
2. **Per-namespace quota eviction.** If the namespace's effective
   policy quota (falling back to the namespace's own `set_quota`) or
   count limit is exceeded, the least valuable live resources are
   evicted until it fits. "Least valuable" is eviction order:
   priority ascending (`Low` before `Normal` before `High` before
   `Critical`), then least-recently-accessed, then id. Only
   `Created`/`Active`/`Locked` records are candidates.
3. **Manager-wide quota eviction** (manager-scoped passes only). Same
   algorithm against the manager-wide limits: the manager policy's
   quota if set, otherwise the `max_size` / `max_resources` from the
   configuration.

One failing resource never stops the pass; the failure lands in
`failed` and the pass continues. Namespace-scoped passes skip phase 3
and limit the scan to the namespace.

## Ttl: Fixed vs Idle

```rust
use cachegirl::{TempManager, Ttl};
use std::time::Duration;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-ttl-{}", std::process::id()));
    let manager = TempManager::builder()
        .root(&dir)
        .default_ttl(Duration::from_secs(3600)) // baked into every record as Ttl::Fixed
        .build()?;
    let ns = manager.namespace("demo")?;

    let meta = manager.registry().get(ns.create_file("probe.bin")?.id())?;
    println!("default ttl: {:?}", meta.ttl);

    println!(
        "fixed expired now? {}",
        Ttl::Fixed(Duration::from_secs(0)).is_expired(meta.created_at, meta.last_accessed, std::time::SystemTime::now())
    );

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

- `Ttl::Fixed(d)` counts from `created_at`. Touching the resource does
  not extend its life. Suitable for "this scratch file is valid for
  five minutes, whatever you do with it".
- `Ttl::Idle(d)` counts from `last_accessed`. Every
  `record_access()`/`cache_get` bumps the clock. Suitable for caches:
  entries die after `d` *without reads*, and active entries live
  indefinitely.
- Boundary semantics: exactly at `ttl` counts as expired.
- A resource whose clock lies in the future (clock skew) is never
  treated as expired — `is_expired` returns false rather than
  reclaiming on a bogus delta.
- `ttl` and `idle_ttl` in `CreateOptions` are mutually exclusive;
  setting both fails at creation. Configuring a TTL through the policy
  and a TTL on the resource is not an error: the resource's own TTL
  wins for that resource.

## Quotas for cleanup

Eviction uses these limits, in order of precedence per namespace:

1. `policy.quota.max_bytes` / `policy.quota.max_resources` of the
   namespace's effective cleanup policy,
2. `NamespaceQuota` from `namespace.set_quota` used as the backstop,

and manager-wide:

1. `CleanupPolicy::quota` of the manager-wide policy,
2. `max_size` / `max_resources` from the builder.

```rust
use cachegirl::{CleanupPolicy, Priority, TempManager};
use std::time::Duration;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-policy-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;

    // sweep after 5 minutes of idleness, cap usage at 64 MiB / 1000
    // resources, and make this the default priority for new resources
    manager.set_cleanup_policy(
        CleanupPolicy::builder()
            .idle_ttl(Duration::from_secs(300))
            .max_size(64 * 1024 * 1024)
            .max_resources(1_000)
            .priority(Priority::Normal)
            .build(),
    );

    let ns = manager.namespace("demo")?;
    let report = ns.create_file("probe.bin")?;
    println!("created {}", report.id());
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Note that quota eviction in a pass nominates victims until the usage
*fits* — i.e. it reclaims live resources to get back under the limit,
and a `Remove` verdict from a custom policy does not exempt a resource
from quota eviction (otherwise the quota would never converge).

## Priority

`Priority::Low` is evicted first, then `Normal` (the default),
`High`, and `Critical` last. Two resources of equal priority evict by
`last_accessed` ascending (stalest first), then by id. Priority only
matters when a quota forces eviction; it never makes a resource expire
earlier under a TTL. `Critical` means "evict me last", not "never".

## Custom cleanup policies

Anything the built-in rules do not nominate — TTL did not lapse, no
quota overflow, nothing pre-marked — can be delegated to your own rule.
A custom policy sees `&ResourceMeta` and a `CleanupContext`
(`now` + which `CleanupScope` the pass is scoped to) and answers
`Keep` or `Remove`. A `Remove` verdict reclaims the resource in the
same pass. Leases still win: a `Remove` on a leased resource defers.

Both styles work — a struct implementing the trait, or a closure:

```rust
use std::sync::Arc;
use cachegirl::{CleanupContext, CreateOptions, PolicyDecision, ResourceMeta, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-custom-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("scratch")?;

    // closure form: closures with this signature implement the trait
    manager.set_custom_cleanup_policy(Arc::new(|meta: &ResourceMeta, _ctx: &CleanupContext| {
        match meta.metadata.get("keep").map(String::as_str) {
            Some("true") => PolicyDecision::Keep,
            _ => PolicyDecision::Remove,
        }
    }));

    ns.create_file_with("precious.bin", CreateOptions::new().metadata("keep", "true"))?;
    ns.create_file("temp.bin")?;

    let report = manager.cleanup_expired()?;
    println!("policy sweep removed {} ({:?})", report.removed, report.reasons.policy);

    manager.clear_custom_cleanup_policy();
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

The closure must be `Send + Sync` (that is part of the closure blanket
impl). A policy is advisory per resource and is called once per record
per pass; keep it cheap and panic-free — the decision is not wrapped in
`catch_unwind`.

Namespace-specific custom policies override the manager-wide one for
that namespace; `set_custom_cleanup_policy` on the namespace and the
manager use the same trait object.

## CleanupReport

Every pass returns a `CleanupReport`:

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-report-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let report = manager.cleanup_expired()?;

    println!("scanned={} removed={}", report.scanned, report.removed);
    println!("skipped={} failed={} deferred={}", report.skipped, report.failed, report.deferred);
    println!("bytes_freed={} duration={:?}", report.bytes_freed, report.duration);
    println!("reasons: ttl_fixed={} ttl_idle={} size_eviction={} count_eviction={} policy={} lease_deferred={} eligible={}",
        report.reasons.ttl_fixed, report.reasons.ttl_idle,
        report.reasons.size_eviction, report.reasons.count_eviction,
        report.reasons.policy, report.reasons.lease_deferred, report.reasons.eligible);

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Field guide:

- `scanned` — records examined (in-scope only).
- `removed` — records actually destroyed and unregistered.
- `skipped` — records the pass looked at but did not act on (still
  valid, or lost a destroy race — someone else won).
- `failed` — records where destruction was attempted and the backend
  refused (see `failure_reasons` for one string per failure).
- `deferred` — destruction attempts held back by active leases.
- `bytes_freed` — sum of the destroyed records' last recorded sizes.
- `reasons` — per-cause breakdown (`RemovalReasons`): `ttl_fixed`,
  `ttl_idle`, `size_eviction`, `count_eviction`, `policy`,
  `lease_deferred`, `eligible`.
- `duration` — wall-clock of the pass.

`CleanupReport::is_quiet()` is true when nothing was removed and
nothing failed (skips and deferrals do not count); `RemovalReasons::is_quiet()`
additionally counts deferrals.

## MAX_CLEANUP_ATTEMPTS

A record whose physical removal fails is re-registered in state
`FailedCleanup` with `cleanup_attempts` bumped. The next pass retries
it; once `cleanup_attempts >= MAX_CLEANUP_ATTEMPTS` (3), the record is
left alone for a human to inspect — the pass skips it, it never loops.
There is no automatic re-queue beyond that; recover by fixing the
underlying cause (permissions, locked file, disk) and then either
calling `release(&id)` again (which resets nothing, but a healthy
backend now succeeds) or restarting the application after manual
removal.

`failure_reasons` collects one human-readable string per failure in
the pass — include it in your log when `failed > 0`, and alert on it if
your service promises bounded garbage.
