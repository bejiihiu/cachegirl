# `no_std` compatibility: audit and roadmap

`cachegirl` is a `std` library. This document records what was audited for
`no_std` compatibility, what is trivially portable, what is structurally
`std`-bound and why, what was changed in the codebase as preparation, and
what a real `no_std` port would require. It is written for users evaluating
embedded or `no_std` deployments and for contributors considering this work.

**Status: `no_std` support is not provided and is not promised for any
release.** Treat `no_std` as future work; the facts below are here so the
decision can be revisited with real data instead of folklore.

## Summary

- The crate compiles against `std` only. Nothing in the public API is
  usable from a `#![no_std]` target today.
- The genuinely portable core (pure data types and the lifecycle state
  machine) is small — on the order of a few types, not a coherent
  standalone product.
- A real port is blocked by public API types, not by implementation detail:
  `ResourceMeta` embeds `SystemTime`, the metadata map is a `std::collections::HashMap`,
  and error enums wrap `std::io::Error`. Fixing those means breaking API
  changes.
- A mechanical `core::` import hygiene pass was done as preparation. It
  changes no behavior, adds no claim of support, and removes the easiest
  obstacle for a future port.
- A satellite crate (`cachegirl-core`) was considered and deferred; see
  [Options considered](#options-considered).

## What "`no_std`" would mean here

Two distinct targets matter, and they are often conflated:

1. **`no_std` + `alloc`** — the target has a global allocator (`Vec`,
   `String`, `Box` work) but no OS services (no threads, no filesystem, no
   clock, no `std::sync` primitives). This is the realistic ceiling for most
   of this crate's bookkeeping logic.
2. **Core-only `no_std`** — no allocator at all (thumbv6m-style targets).
   Only fixed-capacity data (`heapless`-style) is viable. `cachegirl`'s
   registry, metadata maps and reports are inherently growable, so this
   variant is out of scope regardless of effort.

Everything below assumes target (1), the more permissive one.

## Module-by-module audit

### Clean, or nearly clean

| Module / type | Dependencies in a portable build | Verdict |
|---|---|---|
| `core::lifecycle` (`ResourceState`) | none beyond `core` | Portable as-is |
| `policy::quota` (`Quota`) | none; `option::Option::is_none_or`, `u64` | Portable as-is |
| `policy::cleanup` (`Priority`, `CleanupPolicy`, builder) | `core::time::Duration` | Portable; builder uses `core::fmt`-safe paths already |
| `policy::ttl` (`Ttl`, the enum) | `core::time::Duration` | The enum is portable; `Ttl::is_expired` is not (takes `SystemTime`) |
| `policy::custom` (`CleanupScope`, `PolicyDecision`) | enums, `String` (alloc) | Portable; `CleanupContext` is not (`SystemTime`) |
| `cleanup::report` (`RemovalReasons`, `CleanupReport`) | `core::time::Duration`, `Vec`, `core::fmt` | Portable with `alloc` |
| `core::lifecycle::ResourceState` transitions | `core` only | Portable |

### Structurally `std`-bound

| Module / type | Blocking dependency | Why it cannot move without an API break |
|---|---|---|
| `ResourceMeta` (`core::resource`) | `SystemTime`, `std::collections::HashMap<String, String>` | `SystemTime` has no `core` equivalent; the metadata map has no `alloc` home (`std`'s `HashMap` is `RandomState`-gated) |
| `core::resource::ResourceId::new` | `SystemTime::now()`, `std::process::id()` | Wall clock and pid are OS services; needs an injected clock and id source |
| `core::registry::Registry` | `std::sync::Mutex`, `std::collections::HashMap` | `alloc` provides neither; needs `hashbrown` + a critical-section/spin strategy or a platform mutex |
| `policy::ttl::Ttl::is_expired` | `SystemTime::duration_since` | Clock-dependent; needs the same injected time abstraction as `ResourceMeta` |
| `policy::custom::CleanupContext`, `CustomCleanupPolicy` | `SystemTime`, `ResourceMeta` | Inherits both blockers |
| `CleanupEngine` / cleanup collector | `Instant` (pass duration), `SystemTime` (expiry), `Mutex`/`RwLock` | Timing and synchronization are `std` |
| `StatisticsCollector`, `Event`/`VecSink`, `TracingSink` | `SystemTime`, `Mutex` | Same |
| Filesystem backend, recovery, manager root handling, namespace creation | `std::fs`, `std::path`, `std::env`, `std::process`, lock files | OS services by definition; never portable |
| Error enums (`ResourceCreationError`, `BackendError`, `CleanupError`, `ConfigError`, `RecoveryError`) | `std::io::Error`, `PathBuf` | `std::io::Error` exists nowhere in `core`/`alloc`; the error surface would need split variants or an opaque I/O error |
| `catch_unwind` paths (ownership destruction) | `std::panic` | Panic hooks and unwinding are `std` |
| Async façade (`asynchronous` module) | `std::sync::Arc`, executor assumptions | `alloc` has `Arc`, but the façade is meaningless without the `std`-bound core underneath |

### `core` vs `std` facts this rests on (verified against current docs)

- `core::time::Duration` exists in `core` and is the same type re-exported
  by `std::time` — duration math is portable; wall-clock types are not.
- `SystemTime` and `Instant` are `std::time` only. There is no stable
  `core` wall clock or monotonic clock.
- `std::sync::{Mutex, RwLock, Arc}`: `alloc::sync::Arc` exists;
  `Mutex`/`RwLock` do not (they map to OS primitives). `core::sync::atomic`
  does exist.
- `std::collections::HashMap` is not available in `alloc`; `BTreeMap` (alloc)
  or `hashbrown::HashMap` (portable, SwissTable, same ergonomics) are the
  candidates, both of which change the public field/type signatures here.
- `serde` supports `no_std` + `alloc` (`default-features = false`, features
  `alloc` + `derive`). The impedance mismatch is specific impls: the
  `HashMap` and `SystemTime` `Serialize`/`Deserialize` impls are `std`-only.
  `BTreeMap`, `Vec`, `String` and `Duration` serialize fine with `alloc`.
- `thiserror` 2 works in `no_std` (`default-features = false`; needs Rust ≥
  1.81 for `core::error::Error`), but `{path}` shorthand in `#[error(...)]`
  messages and backtrace support require its `std` feature. Our error enums
  additionally embed `std::io::Error`, which no flag removes.
- `heapless` offers fixed-capacity collections without an allocator; they
  trade away `Vec`-like growth and would impose capacity limits on the
  registry and reports. Not a fit for general use of this crate.

## Options considered

### Option A — in-crate `no_std` via a default `std` feature

Sketch: `#![cfg_attr(not(feature = "std"), no_std)]`, `extern crate alloc`,
default feature `std = []`, pure modules compiled always, `std`-touching
modules gated behind `#[cfg(feature = "std")]`.

Why it was rejected for now:

- **Every public struct that embeds `SystemTime` or `HashMap` either gains
  `#[cfg]`-split fields or changes type.** Both are breaking API changes.
- The error enums either lose their `#[from] std::io::Error` variants under
  `no_std` (cfg-dependent public API shape) or stop compiling. Call sites
  matching on variants would silently change meaning across features.
- `serde` coverage splits: with `alloc` + no `std`, `ResourceMeta` cannot
  derive `Serialize`/`Deserialize` at all until the map and clock are fixed.
- The result is two semi-overlapping public APIs selected by a feature flag —
  the worst of both worlds, maintained forever, for users who today just
  need `tempfile` that isn't subprocess-driven.
- Test matrix doubles (`--all-features` already covers several), and the
  `no_std` leg of CI would need a cross target (e.g. `thumbv7em-none-eabi`),
  which this repository's Windows GNU toolchain does not currently install.

Estimated effort: weeks, plus a semver-major release. Risk of half-broken
feature combinations: high.

### Option B — satellite crate `cachegirl-core`

Sketch: extract the portable subset (lifecycle, `Priority`, `Quota`,
`Ttl` decision logic over an injected clock, report types) into a new
`#![no_std]` + `alloc` crate; `cachegirl` depends on it and re-exports.

Why it was deferred rather than done:

- The portable subset has no independent behavior users need on its own:
  it is decision types and bookkeeping shapes. The value appears only when
  paired with a registry and storage, which are exactly the `std`-bound
  parts. Shipping a crate of near-anemic types now optimizes for a use case
  nobody has reported.
- It forks the public API into two versioned crates with lockstep releases,
  before the stable 1.0 API of `cachegirl` itself exists. The split should
  happen deliberately at a release boundary, not as a side effect.
- It does not remove the blockers for a full `no_std` `cachegirl` — it only
  works if someone wants exactly the type vocabulary, which argues for
  waiting for a concrete embedded/no_alloc consumer.

When it becomes the right move: a concrete driver (embedded target, kernel
module, bare-metal service) asks for the portable decision types; or the
`cachegirl` API has stabilized (1.0) and a `#[cfg]`-split refactor can be
landed with clear migration notes.

### Option C — hygiene now, port later (chosen)

Keep one `std` codebase; do the zero-cost mechanical work that makes a
future port cheaper; document the boundary honestly. This is what this
document and the accompanying import cleanup do.

## What was done in this pass

- Migrated import paths for types that live in `core` (`fmt`, `mem`,
  `str::FromStr`, `result::Result`, `time::Duration`) from `std::` to
  `core::` across the crate. Semantics are unchanged (`std` re-exports
  these), so this is invisible to users beyond slightly earlier
  name resolution.
- No `no_std` feature exists; none is advertised; `Cargo.toml` is
  unchanged.
- Verification: `cargo fmt`, `cargo clippy --all-features`, and
  `cargo test --all-features` are green (120 unit + 13 async +
  29 policy/eviction integration + 7 observability + 26 serde + 3 smoke +
  5 doctests). `cargo check --no-default-features` behaves exactly as
  before (4 pre-existing warnings in the lib-test target, and the smoke
  integration test still does not compile without the default `filesystem`
  feature — both unrelated to this change).

Note what this does **not** do: it does not make the crate compile with
`#![no_std]`, and nothing in the build enforces the boundary. It is
preparation, not a feature.

## What a real port requires (checklist for whoever picks this up)

1. Default feature `std = []`; `#![cfg_attr(not(feature = "std"), no_std)]`
   at the crate root; `extern crate alloc`.
2. Time abstraction: `ResourceMeta`, `Ttl::is_expired`, `CleanupContext`,
   `Statistics` timestamps and the cleanup engine must take an injected
   `now: Duration` (or a small `Clock` trait) instead of calling
   `SystemTime::now()`. Public constructors gain parameters or a builder
   knob; this is the unavoidable breaking change.
3. Map decision: `HashMap<String, String>` → `BTreeMap` (alloc) or
   `hashbrown::HashMap` (document the new hasher; order changes either way).
4. Synchronization: `Mutex`/`RwLock` → `hashbrown` locks are data-only;
   the registry/lease/statistics structures need a documented `std` vs
   `no_std` strategy (platform critical section, spin mutex with
   conservative backoff, or `portable-atomic` + interrupt-based
   exclusivity). This is the hardest part on real embedded targets and the
   one most likely to be wrong the first time.
5. Errors: split I/O-carrying variants behind `cfg(feature = "std")` or
   move to an opaque I/O error for the `no_std` build; enable `thiserror`
   with `default-features = false` there; replace `{path}` shorthand with
   `{}` + `.display()`-style formatting (not available in `no_std`).
6. `serde`: `default-features = false, features = ["alloc", "derive"]` for
   the `no_std` build; the map/clock types from (2) and (3) become
   serializable again automatically.
7. Panic paths: `catch_unwind` in ownership destruction is `std`-only;
   decide between disabling last-drop destruction semantics under `no_std`
   (documented) or a `panic = "abort"` contract.
8. CI: a `no_std` check job on a suitable target (e.g.
   `thumbv7em-none-eabihf`), plus `cargo check --no-default-features`;
   document that windows-gnu-like toolchains without a cross target skip
   the bare-metal leg.
9. Docs: feature matrix, `no_std` quick-start, and a statement that the
   filesystem backend and recovery remain `std`-only forever.

## Frequently asked questions

**Can I use `cachegirl` for an embedded project today?**
No — not even the policy types are published as a standalone `no_std`
artifact. You would need the port described above.

**Will the `serde` feature work the same under `no_std`?**
With `alloc` yes in principle, but only after items (2) and (3); the
`HashMap`/`SystemTime` impls are `std`-only. Records written by a `std`
build do roundtrip through an `alloc`-only build once the field types match.

**Why not just gate the three pure modules and call it supported?**
Because the crate's value is the manager: creation, quotas, leases,
cleanup, recovery, observability. Shipping a `no_std` build containing only
`Priority` and `Quota` would be support in name only, and would freeze a
shallow API boundary we would have to carry into the satellite crate later
anyway. Audience for "decision types without a manager" is hypothetical;
the cost is real.

**What about `no_alloc` (core-only) targets?**
Out of scope. Growth in the registry and reports is fundamental; use
`heapless`-based custom bookkeeping if you need that environment.
