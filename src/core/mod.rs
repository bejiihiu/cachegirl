//! Core building blocks: manager, registry, lifecycle, errors.

pub mod error;
pub mod lifecycle;
pub mod manager;
pub mod registry;
pub mod resource;

pub(crate) mod owner;

/// Sum byte counters without panicking on overflow.
///
/// A registry record can legitimately carry `size_bytes == u64::MAX`
/// (e.g. hand-assembled through the public registry API); an ordinary
/// `sum()` would overflow and panic in debug builds. Quota and eviction
/// math must never take the process down, so saturation wins.
pub(crate) fn saturating_bytes(total: impl IntoIterator<Item = u64>) -> u64 {
    total.into_iter().fold(0u64, |acc, n| acc.saturating_add(n))
}
