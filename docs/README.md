# `cachegirl` documentation

A practical index of the guides in this directory. Everything in these
guides compiles against the published API.

## Path for newcomers

Read the guides in this order on first contact:

1. [getting-started.md](getting-started.md) — install, build a manager,
   create and release a file, clean shutdown.
2. [core-concepts.md](core-concepts.md) — the model (Application →
   Manager → Namespace → Resource), the registry, ownership, leases,
   backends, and what the library never does.
3. [lifecycle-management.md](lifecycle-management.md) — the resource
   state machine, who transitions what, and what a cleanup report
   tells you.
4. [configuration.md](configuration.md) — every builder knob, defaults,
   the feature-flag matrix.
5. [resources.md](resources.md) — the resource kinds, handles, `CreateOptions`,
   sizes and `refresh_size`, pooled buffers.
6. [ownership-and-leases.md](ownership-and-leases.md) — who destroys
   what, and when.
7. [cleanup-policies.md](cleanup-policies.md) — TTL, eviction, custom
   policies, `CleanupReport`.
8. [namespaces.md](namespaces.md) — isolation, quotas, nested
   namespaces, per-namespace sweeps.

The rest — crash recovery, schedulers, observability, security,
performance — is reference material you reach for when the deployment
asks for it.

## Guides

| page | what it covers |
| ---- | -------------- |
| [core-concepts.md](core-concepts.md) | the model (Application → Manager → Namespace → Resource), registry and metadata, ownership, leases, lifecycle in brief, backends, no background threads, multiple instances on one root |
| [lifecycle-management.md](lifecycle-management.md) | `Uninitialized`…`Destroyed` state machine, legal transition table, who transitions what, `is_cleanup_eligible`, custom policies and lease-gating, what `CleanupReport` shows |
| [getting-started.md](getting-started.md) | installation, first manager, first namespace and file, explicit cleanup, shutdown |
| [configuration.md](configuration.md) | every builder knob: root, quotas, TTL, symlink policy, instance id, recovery, event sink, cleanup policies, shutdown behaviour; the feature-flag matrix |
| [namespaces.md](namespaces.md) | model, creation, name rules, nested namespaces (paths, depth limit, recursive quotas and cleanup), per-namespace quotas and metadata, `usage()` and `usage_recursive()` |
| [resources.md](resources.md) | the seven resource kinds, handles, `CreateOptions`, paths and sizes, `refresh_size` (observational), pooled buffers, `cache_get` |
| [ownership-and-leases.md](ownership-and-leases.md) | `Owned` / `Shared` / `Managed` / `Leased`, the lease gate, `PendingCleanup`, `active_leases`, panic safety, drop in async code |
| [cleanup-policies.md](cleanup-policies.md) | TTL (`Fixed`/`Idle`), quotas, eviction order and priority, custom policies, `CleanupReport`, `MAX_CLEANUP_ATTEMPTS` |
| [crash-recovery.md](crash-recovery.md) | instance directories, lock file format, staleness decision table, PID reuse, `ReportOnly`, incident checklist |
| [scheduler-integration.md](scheduler-integration.md) | why there is no built-in scheduler; inline sweeps, `std::thread`, `tokio::time::interval`, cron/systemd, per-architecture patterns |
| [sync-and-async.md](sync-and-async.md) | one sync core + an async facade, what offloads to `spawn_blocking`, handles stay sync, feature reference, which to pick |
| [observability.md](observability.md) | `Statistics` snapshots and `render()`, `Event`/`EventSink` (`VecSink`, `NullSink`, `TracingSink`), `metrics` names, `tracing` spans, `CleanupReport` |
| [security.md](security.md) | trust boundaries, name validation, confinement, symlink policy, directory ownership checks, lock files, what is *not* protected, deployment posture |
| [performance.md](performance.md) | architecture and its costs, measured numbers, buffer reuse, batch sweeps, sharding, the unoptimized 10k scan |
| [error-handling.md](error-handling.md) | the nine error categories, `Display`/`source()`, cleanup retry budget, poisoned registry, strategies |
| [writing-a-backend.md](writing-a-backend.md) | the `ResourceBackend` contract, confinement and idempotency requirements, a complete blob-store example, testing |

## Reference material

| page | what it covers |
| ---- | -------------- |
| [benchmarks.md](benchmarks.md) | how the benchmark suite works, methodology, the measured numbers on the author's machine |
| [no-std.md](no-std.md) | honest audit of `no_std` support: what is portable, what is not, and why there is no feature flag for it |

## Finding your way around the API

- `cargo doc --all-features --open` builds the API reference; every
  public item carries doc comments.
- The `examples/` directory contains runnable programs for every major
  feature (`basic`, `namespace`, `ownership`, `custom_backend`,
  `custom_policy`, `custom_scheduler`, `recovery`, `metrics_usage`,
  `tracing_usage`, `async_usage`).
- The crate-level documentation in `src/lib.rs` has a minimal end-to-end
  example and the serialization notes for the `serde` feature.
