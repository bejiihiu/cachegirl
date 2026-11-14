//! Ownership primitives: leases and locks.

mod lease;
mod locking;

pub use lease::{Lease, LeaseBook};
pub use locking::{LockGuard, LockSet};
