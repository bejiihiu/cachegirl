//! User-supplied cleanup decisions.
//!
//! A custom policy is a predicate the cleanup engine consults for
//! resources that are not otherwise eligible for reclamation. It answers
//! one question: should this resource go? Leases still win over the
//! verdict; a policy may nominate a resource for eviction, but a held
//! lease defers the actual destruction.

use std::time::SystemTime;

use crate::core::resource::ResourceMeta;

/// What a cleanup pass was scoped to.
///
/// Serializes as externally tagged, e.g. `"Manager"` or
/// `{"Namespace": "name"}`.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CleanupScope {
    /// Whole manager: every namespace, manager-wide limits enforced.
    Manager,
    /// A single namespace, its own limits and policy enforced.
    Namespace(String),
}

/// Read-only context handed to a custom policy for each decision.
///
/// `now` serializes as `{"secs_since_epoch": ..., "nanos_since_epoch": ...}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CleanupContext {
    /// One `now` for the whole pass, so decisions stay consistent
    /// within a single sweep.
    pub now: SystemTime,
    /// Which scope the pass is operating on.
    pub scope: CleanupScope,
}

/// What a custom policy wants done with a resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PolicyDecision {
    /// Leave the resource alone.
    #[default]
    Keep,
    /// Reclaim the resource in this pass.
    Remove,
}

/// User-defined eviction rule.
///
/// Implement this on your own type, or pass a closure shaped like
/// `|meta: &ResourceMeta, ctx: &CleanupContext| PolicyDecision` —
/// closures implementing that signature get the trait for free.
///
/// The decision is only advisory for lease safety: a `Remove` verdict
/// on a resource with held leases defers destruction instead of forcing it.
pub trait CustomCleanupPolicy: Send + Sync {
    /// Decide whether `resource` may be reclaimed.
    fn decide(&self, resource: &ResourceMeta, context: &CleanupContext) -> PolicyDecision;
}

impl<F> CustomCleanupPolicy for F
where
    F: Fn(&ResourceMeta, &CleanupContext) -> PolicyDecision + Send + Sync,
{
    fn decide(&self, resource: &ResourceMeta, context: &CleanupContext) -> PolicyDecision {
        self(resource, context)
    }
}
