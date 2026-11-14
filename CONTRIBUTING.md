# Contributing to cachegirl

Thanks for your interest in contributing. This document describes how to
build, test, and submit changes.

## Getting started

You need a Rust toolchain installed via
[rustup](https://rustup.rs). The minimum supported Rust version (MSRV) is
**1.85** (Rust edition 2024); CI also builds against stable.

```sh
git clone https://github.com/bejiihiu/cachegirl
cd cachegirl
cargo build
```

## Building and testing

Run the test suite with all features enabled:

```sh
cargo test --all-features
```

The default feature set is `filesystem`; `cargo test` without flags covers
it. `cargo test --doc` runs the doctests, and `cargo bench` runs the
benchmark suite.

Before submitting, make sure the following all pass:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo doc --all-features --no-deps        # with RUSTDOCFLAGS="-D warnings"
```

For changes that touch public API behavior, please also run
`cargo test --no-default-features` and
`cargo test --no-default-features --features filesystem` if the default
feature set is affected.

## Feature flags

See the feature table in the README and in the crate-level rustdoc. Don't
add a feature flag for something that can be a plain dependency; don't add a
dependency for something std can do.

## Style

- Follow the existing style: `rustfmt` defaults, `clippy`-clean with
  `-D warnings`.
- Public items need doc comments (`///`); keep them factual and free of
  marketing language.
- Comments inside the codebase are informal and conversational; user-facing
  text (docs, errors, UI strings, commit messages) is written plainly and
  correctly.
- No `unsafe` — the crate sets `#![forbid(unsafe_code)]`.
- No mocks or stubs in tests; tests must exercise real behavior.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <summary>
```

Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `ci`,
`chore`. Keep the summary line at 50 characters or less. The body, if any,
explains *why*, not what. Don't add trailers unless necessary.

Examples:

```
feat(cleanup): add per-namespace sweep budget
fix(registry): saturate byte accounting on overflow
```

## Pull requests

- Branch off `main`, keep the diff focused, and fill in the PR template.
- One concern per PR. Refactors and behavior changes go in separate PRs.
- CI must be green: fmt, clippy, tests on Linux/Windows/macOS against both
  stable and the MSRV, docs build, no-default-features builds, bench
  compilation and example builds.
- Add tests for behavior changes — bug fixes ship with a regression test.

## MSRV policy

The MSRV is 1.85. CI tests against it; dependency updates that require a
newer compiler will be rejected or deferred. Don't use language features or
APIs newer than the MSRV without bumping it deliberately (and updating this
document along with `rust-version` in `Cargo.toml`).

## Windows (windows-gnu) note

The reference development environment is Windows with the
`*-pc-windows-gnu` target, which has no `dlltool`. Two consequences worth
knowing:

- Some transitive dependencies that expect `dlltool` (e.g. `getrandom`,
  recent `windows-sys`) cannot build there; this crate deliberately avoids
  them. Don't add such dependencies to the default graph.
- `tracing-subscriber` is used without the `ansi` feature and `proptest`
  runs with default features off, for the same reason.

CI runs on GitHub-hosted `windows-latest` (MSVC), where this doesn't apply,
but please keep in mind that the public dependency graph should remain
buildable on `windows-gnu`.

## Signing off

By contributing, you agree that your contributions will be dual-licensed
under the MIT and Apache-2.0 licenses, as described in the README. You don't
need to sign anything.

## Code of conduct and security

Please read [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) and
[SECURITY.md](SECURITY.md). Report vulnerabilities privately through
GitHub Security Advisories — do not open public issues for security
problems.
