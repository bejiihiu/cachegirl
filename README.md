# cachegirl

<p align="center">
  <img src="assets/icon.png" alt="Cachegirl — the cachegirl mascot, a pink girl with a folder" width="200">
</p>

Centralized management of temporary resources in Rust.

`cachegirl` gives your application a single registry for temporary
files, directories, in-memory buffers, cached computation results, and
other short-lived data — with namespaces, quotas, ownership modes, leases,
cleanup policies, and crash recovery. It never spawns background threads,
never requires an async runtime, and never imposes a filesystem layout:
you choose the root, the namespace structure, and when cleanup runs.

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)

## What it does

- **Namespaces** — logical isolation per component, each with its own
  quotas, metadata and resource accounting.
- **Quotas** — manager-wide and per-namespace, by bytes and by resource
  count, enforced atomically at creation time.
- **Ownership modes** — every resource is created with an ownership
  mode: `Owned` and `Shared` handles destroy the resource when the last
  owner drops it, `Managed` and `Leased` resources live until release
  or cleanup.
- **Leases** — RAII leases protect a resource while it is in use.
  Nothing is physically deleted while at least one lease is held;
  release and cleanup move it to `PendingCleanup` instead.
- **Cleanup engine** — explicit, callers-driven reclamation of expired
  and pending resources, with per-resource accounting in the report.
- **Crash recovery** — instance directories and lock files let a new
  process reclaim stale data from crashed runs without touching live
  ones.
- **Observability** — structured events, live statistics (resources,
  bytes, leases, GC runs), optional `metrics` and `tracing` exporters.
- **Pluggable backends** — filesystem and memory backends built in;
  bring your own by implementing the backend contract.

## Feature flags

| feature      | default | what it enables                          |
|--------------|---------|------------------------------------------|
| `filesystem` | yes     | filesystem backend and file resources    |
| `memory`     | no      | memory backend and buffer resources      |
| `cache`      | no      | cache entry resources                    |
| `async`      | no      | async API markers                        |
| `sync`       | no      | sync API markers                         |
| `serde`      | no      | `Serialize`/`Deserialize` for public types |
| `metrics`    | no      | counter export through the `metrics` facade |
| `tracing`    | no      | event emission through `tracing`         |
| `tokio`      | no      | Tokio integration (implies `async`)      |

## Requirements

- Latest stable Rust (MSRV 1.85).
- No mandatory runtime, no global state, no background threads.
- The core builds without the `filesystem` feature; feature-gated
  integrations pull no implicit dependencies.

## Installation

```toml
[dependencies]
cachegirl = { version = "0.1" }
```

## Documentation

- API reference: [docs.rs/cachegirl](https://docs.rs/cachegirl)
- Or build it locally with `cargo doc --all-features --open`.
- Repository: <https://github.com/bejiihiu/cachegirl>

## License

Licensed under either of the MIT license or the Apache License,
Version 2.0, at your option. See `Cargo.toml` for the exact license
declaration.
