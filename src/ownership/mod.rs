//! Ownership primitives: leases and locks.
//!
//! Leases are the primitive building block: while a lease is held the
//! resource cannot be physically destroyed. The concrete resource types
//! (see the `resource` module) hand them out through `acquire()` and
//! defer to them during destruction, release and cleanup passes.

mod core;
mod lease;
mod locking;

pub use lease::{Lease, LeaseBook};
pub use locking::{LockGuard, LockSet};

pub(crate) use core::{DestroyOutcome, OwnershipCore, destroy_resource};
