<table>
<tr>
<td valign="middle">

# cachegirl

[![Rust](https://img.shields.io/badge/Rust-stable-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue?style=flat-square)](https://doc.rust-lang.org/edition-guide/)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue?style=flat-square)](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html)
[![Open Source](https://img.shields.io/badge/open%20source-yes-green?style=flat-square)](https://github.com/bejiihiu/cachegirl)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-yellow?style=flat-square)](https://github.com/bejiihiu/cachegirl)

`cachegirl` gives your application a single registry for temporary files, directories, in-memory buffers, cached computation results, and other short-lived data - with namespaces, quotas, ownership modes, leases, cleanup policies, and crash recovery. It never spawns background threads, never requires an async runtime, and never imposes a filesystem layout: you choose the root, the namespace structure, and when cleanup runs.

</td>
<td width="220" valign="middle" align="center">
<img src="https://raw.githubusercontent.com/bejiihiu/cachegirl/main/assets/icon.png" alt="Cachegirl - the cachegirl mascot, a pink girl with a folder" width="200">
</td>
</tr>
</table>

## What it does

- **Namespaces** - logical isolation per component, each with its own
  quotas, metadata and resource accounting.
- **Quotas** - manager-wide and per-namespace, by bytes and by resource
  count, enforced atomically at creation time.
- **Ownership modes** - every resource is created with an ownership
  mode: `Owned` and `Shared` handles destroy the resource when the last
  owner drops it, `Managed` and `Leased` resources live until release
  or cleanup.
- **Leases** - RAII leases protect a resource while it is in use.
  Nothing is physically deleted while at least one lease is held;
  release and cleanup move it to `PendingCleanup` instead.
- **Cleanup engine** - explicit, callers-driven reclamation of expired
  and pending resources, with per-resource accounting in the report.
- **Crash recovery** - instance directories and lock files let a new
  process reclaim stale data from crashed runs without touching live
  ones.
- **Observability** - structured events, live statistics (resources,
  bytes, leases, GC runs), optional `metrics` and `tracing` exporters.
- **Pluggable backends** - filesystem and memory backends built in;
  bring your own by implementing the backend contract.

## Feature flags

| feature      | default | what it enables                             |
| ------------ | ------- | ------------------------------------------- |
| `filesystem` | yes     | filesystem backend and file resources       |
| `memory`     | no      | memory backend and buffer resources         |
| `cache`      | no      | cache entry resources (implies `memory`)     |
| `async`      | no      | async API facade, runtime-agnostic          |
| `sync`       | no      | sync API markers                            |
| `serde`      | no      | `Serialize`/`Deserialize` for public types  |
| `metrics`    | no      | counters, gauges and histograms through the `metrics` facade |
| `tracing`    | no      | spans and structured events through `tracing`               |
| `tokio`      | no      | Tokio integration: blocking work on the blocking pool (implies `async`) |

## Requirements

- Latest stable Rust (MSRV 1.85).
- No mandatory runtime, no global state, no background threads.
- The core builds without the `filesystem` feature; feature-gated
  integrations pull no implicit dependencies.

## Installation

```sh
cargo add cachegirl
```

## Documentation

- Guides and recipes live in [`docs/`](docs/).
- API reference: [docs.rs/cachegirl](https://docs.rs/cachegirl)
  (build it locally with `cargo doc --all-features --open`).
- Changelog: [CHANGELOG.md](CHANGELOG.md)
- Repository: <https://github.com/bejiihiu/cachegirl>

## License

Licensed under either of [MIT license](LICENSE-MIT) or
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
