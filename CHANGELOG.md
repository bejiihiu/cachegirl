# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-11-14

Initial public release.

### Added

- Centralized temporary-resource management through `TempManager` and a
  builder API: create, register, track, and explicitly reclaim temporary
  files, directories, in-memory buffers, cache entries, serialization
  scratch data, runtime-scoped objects, and user-defined resource types,
  without background threads, hidden schedulers, or a required async
  runtime.
- Scoped creation API on namespaces: `create_file` / `create_directory`
  (and `_with` variants), `create_buffer`, `create_serialization`,
  `create_runtime` / `create_runtime_with`, `create_cache_entry` /
  `create_cache_entry_with` / `cache_get`, and `create_custom` /
  `create_custom_with` through a user-registered backend
  (`TempManager::register_backend`). A single `CreateOptions` object
  carries owner, TTL (`ttl` or mutually exclusive `idle_ttl`),
  ownership, priority and metadata for every creation call.
- Namespaces with per-namespace quotas (bytes and resource count, atomic
  at creation time), metadata, usage accounting, and manager-wide
  defaults. Every resource belongs to exactly one namespace; per-resource
  operations are exact-match and cannot observe other namespaces.
- **Nested namespaces.** `manager.namespace("parent/sub")` and
  `Namespace::namespace("sub")` create hierarchical namespaces; names are
  full paths (`parent/sub`), hierarchy is limited to
  `namespace::MAX_NAMESPACE_DEPTH` (8) segments, every level is
  registered with the manager and materialized on disk under
  `root/<instance>/<path>/`, and quotas/cleanup/statistics see each
  level. New accessors `Namespace::depth`, `parent` and `usage_recursive`.
  - Per-resource access (`release`, `resources`, `usage`, `cache_get`)
    stays exact-match — absolute isolation between namespaces.
  - Quotas recurse: a namespace's bytes/count quota limits the total of
    its whole subtree; enforcement happens at creation against every
    ancestor, atomically, in the registry.
  - Namespace-scoped cleanup recurses: a pass on a parent reclaims the
    parent and every namespace registered under it, each level judged
    against its own policy; ancestor quota eviction picks victims from
    the subtree. Read-only operations do NOT recurse.
  - Children inherit nothing: no quota values, no policies; each level
    falls back to the manager-wide defaults only.
- Ownership modes for every resource: `Owned`, `Shared`, `Managed`, and
  `Leased`. `Owned`/`Shared` handles destroy the resource when the last
  owner drops; `Managed`/`Leased` resources live until release or
  cleanup. Destruction is driven from a single `destroy_resource` path
  shared by drops, releases and cleanup passes.
- RAII leases that gate physical deletion: release and cleanup defer to
  `PendingCleanup` while at least one lease is held. The lease book is
  a manager-wide counter per id with an atomic seal/unseal protocol;
  a lease acquire racing a destruction either wins (destruction defers)
  or loses (the lease never attaches).
- Cleanup engine with Fixed and Idle TTL expiry, byte and count eviction,
  priority ordering (low priority evicted first), and custom cleanup
  policies (closures implementing `CustomCleanupPolicy`), driven
  explicitly by the caller, with a structured `CleanupReport` (scanned,
  removed, failed, deferred, bytes freed, per-reason breakdown in
  `RemovalReasons`). Failed destructions are re-registered in
  `FailedCleanup` and retried up to `MAX_CLEANUP_ATTEMPTS` (3) times.
- Lease-aware shutdown: `TempManagerBuilder::shutdown_wait_leases`
  (also in `ManagerConfig` and the async builder). Shutdown blocks until
  every lease is dropped or the timeout elapses (default `ZERO`, do not
  wait). Resources whose destruction is lease-blocked stay in
  `PendingCleanup` and are counted in the shutdown report's `deferred`.
  Leases are how the library tracks in-flight operations; it does not
  track arbitrary application code — operations that must be seen by
  shutdown must hold a lease.
- Optional instance-directory removal on shutdown:
  `TempManagerBuilder::remove_instance_dir_on_shutdown(bool)` (also in
  `ManagerConfig` and the async builder). After the sweep, shutdown
  deletes `root/<instance>` wholesale. The path must canonically
  resolve strictly inside the canonicalized root — the root itself, a
  planted symlink, or anything resolving at or above the root is
  refused with an error instead of deleting data. Default: off.
- `BufferPool` / `PooledBuffer` (memory feature): a thread-safe pool of
  recycled `Vec<u8>` allocations for hot paths. `acquire` / `acquire_with_capacity`
  hand out cleared buffers, smallest-fit first, growing the smallest
  idle buffer when nothing fits; `with_limits(max_buffers,
  max_buffer_capacity)` tunes retention; both limits accept zero;
  oversized or excess buffers are dropped rather than retained.
  Deliberately not tied to namespaces, managers or quotas — pooled
  buffers are unaccounted scratch space; use `create_buffer` for
  policy-managed accounting.
- Crash recovery: per-instance directories and lock files (PID plus a
  process-lifetime ownership token) let a new process reclaim stale data
  from crashed runs while live instances and healthy locks are left
  untouched. PID-reuse is detected on Linux (`/proc`), Windows (`tasklist`
  plus process start time), and other Unix (`kill -0` + `ps`). Policies:
  `OnStartup` (default), `ReportOnly`, and `Never`;
  `stale_instance_after` tunes the lock-less staleness threshold
  (default 1 hour). Recovery probe failures (unknown liveness) are
  conservative: the directory is kept and reported, never deleted.
- Filesystem backend confined to a canonicalized root: symlink attacks,
  ancestor-symlink escapes, path traversal, reserved device names,
  fullwidth-separator names, and trailing-dot/colon names are rejected;
  deletion is only ever attempted inside the root.
- Memory backend with buffer accounting and configurable byte limits
  (`with_max_bytes`, zero-safe).
- Custom backend contract: implement `ResourceBackend` and register it
  with `TempManager::register_backend` to manage arbitrary resource
  types.
- Async facade (`async` feature) that is runtime-agnostic; the `tokio`
  feature routes blocking work onto the Tokio blocking pool (implies
  `async`). Resource handles stay sync.
- Observability: live statistics (resources, bytes, active leases,
  expired, pending cleanup, GC runs), structured events through a
  pluggable sink (`Event` / `EventSink`; `NullSink`, `VecSink`,
  `TracingSink` with the `tracing` feature), and optional `tracing`
  spans/events and `metrics` counters, gauges, and histograms behind
  feature flags.
- `serde` feature with `Serialize`/`Deserialize` for the public data
  types (`ManagerConfig`, `CreateOptions`, `ResourceMeta`, `CleanupReport`,
  `RecoveryReport`, `Statistics`, events, and others). Unknown fields
  are ignored; enums are externally tagged; older payloads keep
  loading.
- User guides under `docs/` (getting started, core concepts, lifecycle
  management, configuration, namespaces, resources, cleanup policies,
  crash recovery, scheduler integration, sync and async usage,
  observability, security, performance, error handling, writing a
  backend) and a runnable example suite under `examples/`. Guide
  snippets are compile-tested under `cargo test --all-features`.
- Benchmarks (`cargo bench`, std-only harness: allocation, cleanup,
  concurrent, cache, registry) and a property-test suite (`proptest`)
  covering invariants of the registry, quotas, lifecycle table,
  validators, and cleanup report arithmetic.
- `docs/no-std.md`: an honest audit of `no_std` portability (what is
  portable, what is not, why there is no feature flag). No `no_std`
  support is claimed or shipped.
- CI/CD: fmt, clippy (`-D warnings`), test matrix across Linux, Windows
  and macOS, `cargo doc` with `-D warnings`, `no-default-features`
  build, benches-compile check, dependency audit (RustSec), release
  workflow on tags, dependabot.

### Changed

- The `cache` feature now implies `memory`; cache entries are
  accounted through the memory backend with no way to select one
  without the other.
- Resource creation no longer passes through one global creation mutex.
  Quota enforcement is an atomic reservation inside the registry: a
  placeholder record is inserted under the registry lock after the
  manager-wide and every ancestor's quota are checked against internal
  subtree totals. Different namespaces create fully in parallel; a
  shared-quota scope (same namespace, ancestors, or the manager) cannot
  overshoot. Registry totals are maintained incrementally, so
  out-of-band registry mutations can no longer skew quota accounting.
- Namespace-scoped cleanup (`Namespace::cleanup_expired`) sweeps the
  namespace and every registered descendant. Per-resource isolation for
  `release`, `resources`, `usage` and `cache_get` is unchanged
  (exact-match).
- Filesystem backend accepts nested namespace paths (`parent/sub`) and
  materializes the whole subtree confined inside the root, validating
  every path segment.
- `refresh_size` is documented as observational: a resource that grew
  past its quota re-stats successfully and stays tracked; it is
  reclaimed by quota eviction on a later cleanup pass, not by the call.
- `RemovalReasons::destroyed()` uses saturating arithmetic.
- Priority semantics are the standard ones: lower priority is evicted
  first (`Low` before `Normal` before `High` before `Critical`).
- Shutdown sweep runs exactly once, even under concurrent `shutdown()`
  calls; subsequent calls return an empty report.
- Shutdown order is fixed: wait for leases (if configured) → optional
  sweep → lock-file removal → optional instance-directory removal.
- Cleanup destroy-path statistics are counted exactly once:
  `record_release` is the only accounting site for destroyed resources;
  `record_gc_run` only increments `gc_runs`.

### Fixed

- Filesystem release could remove a file outside the managed root when a
  parent directory was replaced by a symlink after creation; existing
  ancestors are now canonicalized and verified against the root before
  deletion.
- Within-root confinement was partially enforced — the `WithinRoot`
  symlink policy behaved as an alias for `Allow`; it now actually
  checks that the canonical target stays inside the root (dangling
  links are refused).
- Cleanup destroyed resources were counted twice in statistics
  (`resources_released`/`bytes_freed`); the report now counts each
  destroyed resource exactly once.
- Shutdown sweep could run more than once under concurrent calls; it now
  runs exactly once and later calls return an empty report.
- Instance identifiers `.`, `..`, and trailing dots passed validation and
  were joined onto the root path; they are now rejected.
- Resource names ending in `.` and names containing `:` were accepted;
  both are now rejected (trailing dots alias two names onto one file on
  Windows; `:` names an alternate data stream on NTFS).
- Byte accounting used a fallible `sum()` in registry, memory backend,
  cleanup collector, and statistics; it now uses saturating arithmetic.
- Lock files written during recovery could be redirected outside the root
  by a planted symlink on the instance directory; the instance directory
  is now canonicalized before the lock is written.
- Linux liveness probe returned a wrong result class when `/proc/self/stat`
  parsing failed mid-probe; probe failures now conservatively report
  "unknown" (recovery keeps the directory).

### Security

- Within-root confinement is enforced for create, release, and cleanup,
  including symlink-ancestor and planted-lock-file scenarios.
- On Unix, an existing `root` (or `root/<instance_id>`) owned by a
  different uid fails the build with `ConfigError::DirNotOwned` instead
  of being adopted; symlinks are judged by their target, and an
  undefined current uid fails closed. On Windows only existence can be
  checked — the standard library exposes no ownership primitive there.
- Instance-directory removal on shutdown is confined to the
  canonicalized root: the canonical target is removed, never a planted
  symlink at the instance path, and never anything at or above the root.
- Recovery never deletes a directory whose lock indicates a live process
  or an ownership token that does not match; unreadable or
  misplaced locks are reported, never removed; probe results of
  "unknown" are conservative (kept, reported).
- Creation-time quota accounting replaces the creation mutex with a
  registry-atomic reservation, eliminating a check-then-act race that
  could overshoot shared quotas under concurrent creation.
- Names are validated at the API boundary — resource names, namespace
  names (per path segment), and instance ids reject traversal,
  control characters, reserved device names (case-insensitive, any
  extension), trailing dots and colons, with typed errors; nothing is
  silently sanitized.
- Quota and cleanup eviction paths use saturating arithmetic, so
  `u64::MAX` sizes and limits do not panic.

### Notes

- Minimum supported Rust version (MSRV) is **1.85** (edition 2024).
- Licensed under either of MIT or Apache-2.0, at your option.
- No mandatory runtime, no background threads, no global state; multiple
  independent `TempManager` instances can coexist on one root, each in
  its own instance directory with its own lock file.
- Not claimed and not shipped: `no_std` support. See `docs/no-std.md`.

[1.0.0]: https://github.com/bejiihiu/cachegirl/releases/tag/v1.0.0
