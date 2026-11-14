# Namespaces

Namespaces are the isolation boundary of the whole design. Every
resource belongs to exactly one namespace; every namespace lives inside
exactly one manager. A namespace carries its own quota, its own metadata
and its own cleanup policy, and none of its operations can observe or
disturb resources belonging to another namespace. Namespaces may nest:
`manager.namespace("parent/sub")` creates `sub` below `parent`, and a
child namespace behaves like any other — with the quota and cleanup
rules described below.

## Creating namespaces

Namespaces are created lazily on first use and are cheap to clone
(`Arc` inside). The same call returns the same object:

```rust
use cachegirl::{NamespaceQuota, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-ns-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;

    let network = manager.namespace("network")?;
    let again = manager.namespace("network")?;
    assert!(std::sync::Arc::ptr_eq(&network, &again));

    network.set_quota(NamespaceQuota::with_max_bytes(4096));
    println!("network quota: {:?}", network.quota());

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

The namespace directory is materialized at `root/<instance>/<name>/` on
first creation. `manager.namespaces()` lists the names created so far.
Creating a namespace after `shutdown()` fails with
`Error::Concurrency(ConcurrencyError::ManagerClosed)`.

## Nested namespaces

A namespace can hold child namespaces. Two equivalent calls create
`parent/sub`:

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-nested-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;

    let packets = manager.namespace("network/packets")?;   // manager path form
    let parent = manager.namespace("network")?;
    let again = parent.namespace("packets")?;              // child form — same namespace
    assert!(std::sync::Arc::ptr_eq(&packets, &again));

    println!("name={} depth={} parent={:?}", packets.name(), packets.depth(), packets.parent());

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

The rules, in full:

- **Names are full paths.** `packets.name()` is `network/packets`;
  `packets.depth()` is 2; `packets.parent()` is `Some("network")`.
  The child's directory is `root/<instance>/network/packets/`.
- **Paths are validated per segment.** `/` separates segments; every
  segment obeys the same rules as a top-level namespace (charset, no
  `.`/`..`, no trailing `.`, no reserved device names). Empty segments
  (`a//b`, leading or trailing `/`) and anything over
  `MAX_NAMESPACE_DEPTH` (8) segments are rejected with
  `Error::Config(ConfigError::InvalidNamespaceName(path))` — the error
  echoes the full path, not just the offending segment. A plain flat
  name is a valid one-segment path.
- **Depth is capped at 8.** Top-level namespaces are depth 1. Creating
  beyond the limit is rejected.
- **Intermediate levels are materialized.** Creating `a/b/c` registers
  `a` and `a/b` with the manager as real namespaces (with their own
  quotas, metadata and policies), and creates the on-disk directories
  along the path. They are not placeholders; the manager's map, usage
  accounting, statistics and cleanup see them like any other namespace.
- **Quotas recurse down the subtree.** A namespace's bytes/count quota
  limits the total of itself and all registered descendants. Creation
  checks the reservation against every ancestor atomically in the
  registry, so a child can never grow past what its parent or the
  manager allows — even under concurrent creation from many threads.
- **Namespace-scoped cleanup recurses.** `ns.cleanup_expired()`
  reclaims the namespace and every namespace registered under it; each
  level is judged against its own policy, and ancestor quota eviction
  picks victims from the whole subtree.
- **Per-resource isolation stays exact-match.** `release`,
  `resources()`, `usage()` and `cache_get` see only the namespace
  named — never the parent, never the children. `usage_recursive()`
  exists for the subtree total.
- **Nothing is inherited.** Children do not inherit the parent's quota
  value or cleanup policy; each level falls back to the manager-wide
  defaults only.
- **One name, one directory.** The namespace map is keyed by full
  path, so `manager.namespace("network/packets")` and
  `parent.namespace("packets")` always return the same `Arc<Namespace>`.

## Name rules

Namespace names are validated at the API boundary:

- non-empty, ASCII alphanumeric plus `-`, `_` and `.`;
- `.` and `..` are rejected;
- no `/` or `\` (they would traverse the root);
- no trailing `.` (Windows strips trailing dots, which would alias two
  names onto one directory);
- no leading or trailing whitespace;
- reserved device names are rejected case-insensitively: `CON`, `PRN`,
  `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, with any extension;
- a leading `.` is fine (`.hidden` is a valid namespace name).

Violations are rejected with `Error::Config(ConfigError::InvalidNamespaceName(name))`.

Pick boring names. A namespace name becomes a directory name on disk,
shows up in metrics labels (`cachegirl_resources_created_total{namespace=...}`)
and in log lines, so keep it short, lowercase and stable. Do not put
user-controlled input into namespace names — validate first, or use a
fixed set of known namespaces and treat everything else as an error.

## Per-namespace quotas

`set_quota` replaces the namespace's quota; `quota()` reads it. Three
constructors cover the common shapes:

```rust
use cachegirl::{NamespaceQuota, TempManager};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-quota-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("scratch")?;

    ns.set_quota(NamespaceQuota::with_max_bytes(8 * 1024 * 1024)); // bytes only
    ns.set_quota(NamespaceQuota::with_max_resources(100));         // count only
    ns.set_quota(NamespaceQuota::with_limits(8 * 1024 * 1024, 100)); // both
    ns.set_quota(NamespaceQuota::unlimited());                      // clear

    println!("usage: {:?}", ns.usage()?);
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

Namespace quotas are enforced atomically at creation time, on top of the
manager-wide `max_size` / `max_resources`. Creation fails with
`Error::Quota(QuotaError::SizeExceeded { limit, requested })` or
`Error::Quota(QuotaError::CountExceeded { limit })`. For nested
namespaces the check runs against every ancestor: a namespace's quota
limits the sum of its whole subtree, and the reservation fails if any
level on the path would be exceeded. Quotas are also used for eviction
during cleanup passes (see [cleanup-policies.md](cleanup-policies.md));
the quota that evicts is `policy.quota` if the effective policy sets one,
otherwise the namespace's own `set_quota` value.

## Measuring usage

`usage()` returns `(bytes, count)` for live resources in this namespace
only — it reads the registry snapshot, so it is exact but not free
(proportional to the number of registered records). `usage_recursive()`
returns the same pair for the whole subtree (this namespace plus every
registered descendant). `resources()` returns the full `Vec<ResourceMeta>`
records for this namespace only, which is what you want for a diagnostic
dump or a watchdog tick.

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-usage-{}", std::process::id()));
    let manager = TempManager::builder().root(&dir).build()?;
    let ns = manager.namespace("scratch")?;

    let file = ns.create_file("probe.bin")?;
    std::fs::write(file.path(), b"abc").expect("file is writable");
    ns.refresh_size(file.id())?;

    let (bytes, count) = ns.usage()?;
    println!("bytes={bytes} count={count}");
    assert_eq!(bytes, 3);

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

File sizes are not synced automatically (creation registers size 0);
call `refresh_size` after writing, or use buffers, whose sizes are known
at creation time. `usage()`'s byte count reflects whatever was last
recorded — it is a floor, not necessarily the on-disk truth.

## Metadata on the namespace

`set_metadata(key, value)` / `metadata(key)` attach free-form strings to
the namespace itself — team, purpose, owner ticket, whatever the
application needs to grep for later. Namespace metadata is in-memory
only; it does not persist across restarts. Resource-level metadata
(`CreateOptions::metadata`) is recorded in the registry record and does
survive in reports.

## Isolation guarantees

- `namespace.release(id)` with an id created in another namespace
  returns `Error::Registry(RegistryError::NotFound(id))` — the resource
  is invisible, not just protected.
- `resources()` and `usage()` only see this namespace's records;
  `usage_recursive()` is the explicit opt-in for the subtree total.
- A namespace-scoped `cleanup_expired()` scans the namespace and every
  registered descendant — and nothing else. It never touches sibling or
  ancestor branches and never applies manager-wide limits.
- The registry itself is shared, but every public operation is
  namespace-scoped or manager-scoped — there is no cross-namespace
  handle in the API.

## Namespace-scoped cleanup

`namespace.cleanup_expired()` runs the same three-phase pass as the
manager-wide one, but:

- the scan covers the namespace and every namespace registered under it
  (each level is judged against its own policy);
- namespace-specific policies and each namespace's own quota are enforced
  per level, and ancestor quota eviction picks victims from the whole
  subtree when an ancestor is over its limit;
- manager-wide limits are not applied (that is what the manager pass is
  for);
- the report's scope label is `namespace` (relevant for the
  `cachegirl_cleanup_runs_total{scope=...}` metric).

Read-only operations (`release`, `resources`, `usage`) are never
recursive — they stay exact-match on the namespace you called them on.

Use a namespace-scoped sweep when namespaces have very different
lifetimes — e.g. sweep a `jobs` namespace every 100 ms but a `config`
namespace once an hour, from different timers. Passing a parent sweeps
its whole subtree, which is what you want when a top-level component
owns a hierarchy of scratch areas.

## Effective cleanup policy

`namespace.cleanup_policy()` resolves in this order: the
namespace-specific policy set through `set_cleanup_policy`, otherwise
the manager-wide policy, otherwise an unlimited default. Namespaces can
also carry their own custom policy through `set_custom_cleanup_policy`,
which overrides the manager-wide custom policy for this namespace.
`clear_cleanup_policy` / `clear_custom_cleanup_policy` remove the
namespace-specific layer. Details and eviction order: see
[cleanup-policies.md](cleanup-policies.md).

## A note on sharding hot paths

A namespace is not a contention unit — the registry has a single mutex
and quota checks serialize across all namespaces of a manager. If a
hot path hammers one namespace, splitting it into more namespaces does
not increase creation throughput. Namespaces buy you isolation and
independent quotas; they do not buy you parallelism. What they do give
you is independent sweeps and independent reclamation schedules, which
is the usual reason to split.
