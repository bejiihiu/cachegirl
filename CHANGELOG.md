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
- Namespaces with per-namespace quotas (bytes and resource count, atomic at
  creation time), metadata, usage accounting, and manager-wide defaults.
- Ownership modes for every resource: `Owned`, `Shared`, `Managed`, and
  `Leased`. `Owned`/`Shared` handles destroy the resource when the last
  owner drops; `Managed`/`Leased` resources live until release or cleanup.
- RAII leases that gate physical deletion: release and cleanup defer to
  `PendingCleanup` while at least one lease is held.
- Cleanup engine with Fixed and Idle TTL expiry, byte and count eviction,
  priority ordering (low priority evicted first), and custom cleanup
  policies (closures implementing `CustomCleanupPolicy`), driven explicitly
  by the caller, with a structured `CleanupReport` (scanned, removed,
  failed, deferred, bytes freed, per-reason breakdown).
- Crash recovery: per-instance directories and lock files (PID plus a
  process-lifetime ownership token) let a new process reclaim stale data
  from crashed runs while live instances and healthy locks are left
  untouched. PID-reuse is detected on Linux (`/proc`), Windows (`tasklist`
  plus process start time), and other Unix (`kill -0` + `ps`). Policies:
  `OnStartup` (default) and `ReportOnly`.
- Filesystem backend confined to a canonicalized root: symlink attacks,
  ancestor-symlink escapes, path traversal, reserved device names,
  fullwidth-separator names, and trailing-dot/colon names are rejected;
  deletion is only ever attempted inside the root.
- Memory backend with buffer accounting and configurable byte limits.
- Custom backend contract: implement `ResourceBackend` and register it with
  `TempManager::register_backend` to manage arbitrary resource types.
- Async facade (`async` feature) that is runtime-agnostic; the `tokio`
  feature routes blocking work onto the Tokio blocking pool (implies
  `async`).
- `serde` feature with `Serialize`/`Deserialize` for the public data types
  (`ManagerConfig`, `CreateOptions`, `ResourceMeta`, `CleanupReport`,
  `RecoveryReport`, and others).
- Observability: live statistics (resources, bytes, active leases, GC
  runs), structured events through a pluggable sink, and optional `tracing`
  spans/events and `metrics` counters, gauges, and histograms behind
  feature flags.
- User guides under `docs/` (getting started, configuration, namespaces,
  resources, cleanup policies, crash recovery, scheduler integration, sync
  and async usage, observability, security, performance, error handling,
  writing a backend) and a runnable example suite under `examples/`.
- Benchmarks (`cargo bench`, std-only harness) and a property-test suite
  (`proptest`) covering invariants of the registry, quotas, lifecycle
  table, validators, and cleanup report arithmetic.

### Fixed

- Filesystem release could remove a file outside the managed root when a
  parent directory was replaced by a symlink after creation; existing
  ancestors are now canonicalized and verified against the root before
  deletion.
- Cleanup destroyed resources were counted twice in statistics
  (`resources_released`/`bytes_freed`); the report now counts each
  destroyed resource exactly once.
- Shutdown sweep could run more than once under concurrent calls; it now
  runs exactly once and later calls return an empty report.
- Instance identifiers `.`, `..`, and trailing dots passed validation and
  were joined onto the root path; they are now rejected.
- Byte accounting used a fallible `sum()` in registry, memory backend,
  cleanup collector, and statistics; it now uses saturating arithmetic.
- Lock files written during recovery could be redirected outside the root
  by a planted symlink on the instance directory; the instance directory
  is now canonicalized before the lock is written.
- Resource creation rejected names ending in `.` and names containing `:`.

### Security

- Within-root confinement is enforced for create, release, and cleanup,
  including symlink-ancestor and planted-lock-file scenarios.
- Recovery never deletes a directory whose lock indicates a live process
  or an ownership token that does not match; unreadable or
  misplaced locks are reported, never removed.
- Quota and cleanup eviction paths use saturating arithmetic, so
  `u64::MAX` sizes and limits do not panic.

### Notes

- Minimum supported Rust version (MSRV) is **1.85** (edition 2024).
- Licensed under either of MIT or Apache-2.0, at your option.
- No mandatory runtime, no background threads, no global state; multiple
  independent `TempManager` instances can coexist on one root.

[1.0.0]: https://github.com/bejiihiu/cachegirl/releases/tag/v1.0.0
