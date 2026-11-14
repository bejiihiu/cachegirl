//! Leases: short-lived rights to use a resource.
//!
//! While at least one lease is held, the resource must not be deleted.
//! Leases are counted in a shared book; dropping a lease releases it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::error::{ConcurrencyError, RegistryError};
use crate::core::resource::ResourceId;

/// Shared ledger of active leases, reference-counted for cheap cloning.
#[derive(Default)]
pub struct LeaseBook {
    inner: Mutex<HashMap<ResourceId, usize>>,
    max_per_resource: Option<usize>,
}

impl LeaseBook {
    /// A book with no per-resource lease limit.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A book that allows at most `max` leases per resource.
    pub fn with_limit(max: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            max_per_resource: Some(max),
        })
    }

    /// Acquire a lease on `id`.
    pub fn acquire(self: &Arc<Self>, id: &ResourceId) -> Result<Lease, ConcurrencyError> {
        let mut map = self.inner.lock().map_err(|_| ConcurrencyError::Poisoned)?;
        let count = map.entry(id.clone()).or_insert(0);
        if let Some(max) = self.max_per_resource {
            if *count >= max {
                return Err(ConcurrencyError::LeaseLimit(id.clone()));
            }
        }
        *count += 1;
        Ok(Lease {
            id: id.clone(),
            book: Arc::downgrade(self),
        })
    }

    /// Number of active leases on a resource.
    pub fn lease_count(&self, id: &ResourceId) -> Result<usize, RegistryError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .get(id)
            .copied()
            .unwrap_or(0))
    }

    /// Total active leases across all resources.
    pub fn total_leases(&self) -> Result<usize, RegistryError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .values()
            .sum())
    }

    fn release(&self, id: &ResourceId) {
        if let Ok(mut map) = self.inner.lock() {
            if let Some(count) = map.get_mut(id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(id);
                }
            }
        }
    }
}

/// RAII lease handle. Dropping it releases the lease.
pub struct Lease {
    id: ResourceId,
    book: Weak<LeaseBook>,
}

impl Lease {
    /// The resource this lease protects.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(book) = self.book.upgrade() {
            book.release(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_block_until_dropped() {
        let book = LeaseBook::new();
        let id = ResourceId::new();
        assert_eq!(book.lease_count(&id).unwrap(), 0);
        let l1 = book.acquire(&id).unwrap();
        let l2 = book.acquire(&id).unwrap();
        assert_eq!(book.lease_count(&id).unwrap(), 2);
        drop(l1);
        assert_eq!(book.lease_count(&id).unwrap(), 1);
        drop(l2);
        assert_eq!(book.lease_count(&id).unwrap(), 0);
    }

    #[test]
    fn per_resource_limit_is_enforced() {
        let book = LeaseBook::with_limit(1);
        let id = ResourceId::new();
        let _l = book.acquire(&id).unwrap();
        assert!(matches!(
            book.acquire(&id),
            Err(ConcurrencyError::LeaseLimit(_))
        ));
    }
}
