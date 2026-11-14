# Configuration

Everything about a manager is decided at build time through
`TempManager::builder()` (or `AsyncTempManager::builder()`, which accepts
the same knobs). This page documents every knob on the builder today, its
default, and when you would change it.

## Feature flags

Features are additive. The default build enables only `filesystem`.

| feature      | default | what it enables                                                        |
| ------------ | ------- | ---------------------------------------------------------------------- |
| `filesystem` | yes     | filesystem backend, file and directory resources, startup recovery     |
| `memory`     | no      | memory backend, buffer / serialization / runtime resources              |
| `cache`      | no      | cache entry resources (requires `memory` for the backing backend)      |
| `async`      | no      | async facade (`AsyncTempManager`, runtime-agnostic)                    |
| `sync`       | no      | reserved marker; the sync API is always available                      |
| `serde`      | no      | `Serialize`/`Deserialize` for data-shaped public types                 |
| `metrics`    | no      | counters, gauges and histograms through the `metrics` facade           |
| `tracing`    | no      | spans and structured events through `tracing`                          |
| `tokio`      | no      | Tokio integration: blocking operations on the blocking pool; implies `async` |

`cache` entries are accounted through the memory backend, so build with
`--features cache,memory` (or `--all-features`) to create cache entries.

## Builder reference

```rust
use std::sync::Arc;
use std::time::Duration;

use cachegirl::{
    CleanupPolicy, NullSink, RecoveryPolicy, SymlinkPolicy, TempManager,
};

fn main() -> cachegirl::Result<()> {
    let dir = std::env::temp_dir().join(format!("cachegirl-config-{}", std::process::id()));

    let manager = TempManager::builder()
        .root(&dir)                                  // required
        .create_if_missing(true)                     // default true
        .cleanup_on_start(true)                      // default false
        .cleanup_on_shutdown(true)                   // default false
        .max_size(1024 * 1024 * 1024)                // default: unlimited
        .max_resources(10_000)                       // default: unlimited
        .default_ttl(Duration::from_secs(3600))      // default: no TTL
        .symlink_policy(SymlinkPolicy::Reject)       // default: Reject
        .instance_id("my-service-1")                 // default: random
        .recovery_policy(RecoveryPolicy::OnStartup)  // default: OnStartup
        .stale_instance_after(Duration::from_secs(3600)) // default: 1 hour
        .event_sink(Arc::new(NullSink))              // default: NullSink
        .cleanup_policy(CleanupPolicy::builder().max_resources(100).build())
        .build()?;

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

This is the complete surface of `TempManagerBuilder`. The table below
explains each one.

### `root(path)` — required

Directory everything lives under. It is canonicalized at build time; all
confinement checks compare canonical paths. If the directory does not
exist, the build fails unless `create_if_missing(true)` (the default).
The root should be dedicated to this library — recovery treats every
directory directly inside it as an instance candidate and ignores
top-level files. Pointing the root at a directory shared with untrusted
writers is unsupported; see [security.md](security.md).

### `create_if_missing(bool)` — default `true`

Create `root` (and the instance directory) when missing. Set to `false`
if you want a missing root to be a hard configuration error instead of a
silent `mkdir -p`.

### `cleanup_on_start(bool)` — default `false`

When enabled, `build()` runs a recovery pass over the root before
handing the manager to your application: stale instance directories from
crashed runs are classified and (depending on `recovery_policy`)
reclaimed. The pass never aborts the build; problems are collected into
`TempManager::recovery_report()` and logged through the
event sink. See [crash-recovery.md](crash-recovery.md).

### `cleanup_on_shutdown(bool)` — default `false`

When enabled, `shutdown()` runs one `cleanup_expired()` sweep before
removing the instance lock file. Shutdown stays safe to call multiple
times; only the first call sweeps.

### `max_size(u64)` / `max_resources(u64)` — defaults: unlimited

Manager-wide quotas, enforced atomically at creation time (one global
creation lock keeps the check-and-register pair race-free). Zero is
rejected as a configuration error. Per-namespace quotas
(`Namespace::set_quota`) are enforced on top of these. Both are
saturating: byte accounting never wraps, so `u64::MAX` is "effectively
forever" without panicking.

### `default_ttl(Duration)` — default: none

A fixed-from-creation TTL applied to every resource that does not set
its own `CreateOptions::ttl`/`idle_ttl`. Per-resource options override
the manager default; per-resource options also win over the cleanup
policy's TTL. The effective TTL at cleanup time is resolved in this
order:

1. the resource's own `ttl` (from `CreateOptions`),
2. the effective policy of its namespace,
3. the manager-wide cleanup policy,
4. the manager's `default_ttl` only at creation time (it is baked into
   the registry record's `ttl` field as `Ttl::Fixed`).

### `symlink_policy(SymlinkPolicy)` — default `Reject`

How the filesystem backend treats symlinks in paths it touches:

- `Reject` — refuse to operate on a symlinked path. Safest.
- `WithinRoot` — allow symlinks whose canonical target stays inside the
  canonical root. Dangling links are refused.
- `Allow` — follow symlinks anywhere. Only for roots you fully trust.

### `instance_id(String)` — default: random

This instance's directory name under `root` and the value other tools
(and recovery) use to attribute the directory to a run. Must match
`[A-Za-z0-9._-]+`, must not be `.`/`..`, must not end with `.`, must not
be a reserved device name. A random id looks like `instance-<nanos>-<tick>`.

### `recovery_policy(RecoveryPolicy)` — default `OnStartup`

Only meaningful with `cleanup_on_start(true)`:

- `OnStartup` — remove directories judged certainly stale.
- `ReportOnly` — classify everything, delete nothing. Good for a first
  deployment on a populated root.
- `Never` — do not scan at all; `recovery_report()` is `None`.

### `stale_instance_after(Duration)` — default `1 hour`

Age after which a directory *without* a lock file is considered stale by
recovery. Directories *with* a lock file are judged by pid + lifetime
token instead; this threshold never applies to them. Setting it to zero
means "a lock-less directory is stale immediately" — useful for tests,
not for production.

### `event_sink(Arc<dyn EventSink>)` — default `NullSink`

Destination for structured lifecycle events. `VecSink` records events in
memory (useful in tests), `NullSink` drops them, `TracingSink` (feature
`tracing`) emits them as debug records. A sink is called synchronously
from the emitting operation, including from destructors; it must be
cheap and it must not panic. See [observability.md](observability.md).

### `cleanup_policy(CleanupPolicy)` — default: none

Manager-wide cleanup policy: TTL sweep rules, the aggregate quota used
for eviction (bytes and/or count), and the default eviction priority
for newly created resources. See [cleanup-policies.md](cleanup-policies.md).
Namespace-specific policies override the manager-wide one for resources
of that namespace.

### `custom_cleanup_policy(Arc<dyn CustomCleanupPolicy>)` — default: none

Manager-wide custom policy, consulted for resources not otherwise
eligible for reclamation. A `Remove` verdict reclaims in the same pass;
a held lease still defers destruction. See
[cleanup-policies.md](cleanup-policies.md).

### Runtime policy changes

`cleanup_policy()`, `set_cleanup_policy()`, `clear_cleanup_policy()`,
`set_custom_cleanup_policy()` and `clear_custom_cleanup_policy()` are
available on the manager (and async variants) to change policies after
the build, e.g. from a control plane or a feature flag. They are stored
behind an `RwLock` and take effect on the next pass.

## Serialization of the config

With the `serde` feature, `ManagerConfig` derives `Serialize`/`Deserialize`
as a flat object; `PathBuf` fields are strings (so paths must be UTF-8 to
roundtrip) and missing fields with a default are filled in —
`create_if_missing` (true), `cleanup_on_start`/`cleanup_on_shutdown`
(false), `symlink_policy`, `recovery_policy` and `stale_instance_after`
(1 hour). `root` and `instance_id` are required. Unknown fields are
ignored. This makes it straightforward to persist a generated config for
a "write the effective configuration to disk at startup" audit log.

## Checking what you built

`manager.config()` returns `&ManagerConfig` — the validated, canonicalized
configuration as actually used. `manager.instance_id()`, `manager.root()`
and `manager.is_closed()` are the same accessors the async facade
re-exports.

## Which knobs interact

- `recovery_policy` and `cleanup_on_start` do nothing without each other.
- `default_ttl` interacts with `cleanup_on_shutdown(true)`: a shutdown
  sweep only removes what has already outlived its TTL.
- `max_size` / `max_resources` bound the manager; `Namespace::set_quota`
  bounds one namespace; both must pass at creation. Eviction during a
  cleanup pass considers policy quotas, namespace quotas and manager
  quotas (see the cleanup pipeline in [cleanup-policies.md](cleanup-policies.md)).
- `symlink_policy` only affects the filesystem backend; custom backends
  make their own confinement decisions.
