//! Try-locks for exclusive operations on a resource.
//!
//! Non-blocking only: if someone else holds the lock, you get an error
//! instead of waiting. That keeps lock convoys out of the hot path.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;

/// Shared set of currently locked resources.
#[derive(Default)]
pub struct LockSet {
    inner: Mutex<HashSet<ResourceId>>,
}

impl LockSet {
    /// Create an empty lock set.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Try to lock a resource. Fails with `Locked` if already held.
    pub fn try_acquire(self: &Arc<Self>, id: &ResourceId) -> Result<LockGuard, ConcurrencyError> {
        let mut set = self.inner.lock().map_err(|_| ConcurrencyError::Poisoned)?;
        if !set.insert(id.clone()) {
            return Err(ConcurrencyError::Locked(id.clone()));
        }
        Ok(LockGuard {
            id: id.clone(),
            set: Arc::downgrade(self),
        })
    }

    /// True while a resource is locked by someone.
    pub fn is_locked(&self, id: &ResourceId) -> bool {
        self.inner.lock().map(|s| s.contains(id)).unwrap_or(false)
    }

    fn release(&self, id: &ResourceId) {
        if let Ok(mut set) = self.inner.lock() {
            set.remove(id);
        }
    }
}

/// RAII lock guard. Dropping it releases the lock.
pub struct LockGuard {
    id: ResourceId,
    set: Weak<LockSet>,
}

impl LockGuard {
    /// The resource this guard locks.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(set) = self.set.upgrade() {
            set.release(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_attempt_fails_until_guard_drops() {
        let set = LockSet::new();
        let id = ResourceId::new();
        let guard = set.try_acquire(&id).unwrap();
        assert!(set.is_locked(&id));
        assert!(matches!(
            set.try_acquire(&id),
            Err(ConcurrencyError::Locked(_))
        ));
        drop(guard);
        assert!(!set.is_locked(&id));
        assert!(set.try_acquire(&id).is_ok());
    }

    #[test]
    fn locks_on_different_ids_do_not_interfere() {
        let set = LockSet::new();
        let a = ResourceId::new();
        let b = ResourceId::new();
        let guard_a = set.try_acquire(&a).unwrap();
        let guard_b = set.try_acquire(&b).unwrap();
        assert!(set.is_locked(&a));
        assert!(set.is_locked(&b));
        drop(guard_a);
        assert!(!set.is_locked(&a));
        assert!(set.is_locked(&b));
        drop(guard_b);
        assert!(!set.is_locked(&b));
    }

    #[test]
    fn relock_after_drop_is_repeatable() {
        let set = LockSet::new();
        let id = ResourceId::new();
        for round in 0..3 {
            let guard = set.try_acquire(&id).unwrap();
            assert!(set.is_locked(&id), "round {round}");
            assert!(set.try_acquire(&id).is_err());
            drop(guard);
            assert!(!set.is_locked(&id), "round {round}");
        }
    }

    #[test]
    fn guard_reports_the_resource_it_locks() {
        let set = LockSet::new();
        let id = ResourceId::new();
        let guard = set.try_acquire(&id).unwrap();
        assert_eq!(guard.id(), &id);
    }
}
