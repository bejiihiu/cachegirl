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
}
