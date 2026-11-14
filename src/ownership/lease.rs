//! Leases: short-lived rights to use a resource.
//!
//! While at least one lease is held, the resource must not be deleted.
//! Leases are counted in a shared book; dropping a lease releases it.
//!
//! The book is also the gate that keeps destruction honest: deleting a
//! resource seals it inside the book first (one mutex, one indivisible
//! step), so an `acquire` that races a deletion either lands before the
//! seal or fails with [`ConcurrencyError::Destroying`] — never both.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use crate::core::error::{ConcurrencyError, RegistryError};
use crate::core::resource::ResourceId;
use crate::observability::StatisticsCollector;
use crate::observability::metrics_support;

#[derive(Default)]
struct BookInner {
    counts: HashMap<ResourceId, usize>,
    /// Ids whose physical destruction is in progress; new leases refused.
    sealed: HashSet<ResourceId>,
}

/// Shared ledger of active leases, reference-counted for cheap cloning.
pub struct LeaseBook {
    inner: Mutex<BookInner>,
    max_per_resource: Option<usize>,
    counters: Arc<StatisticsCollector>,
}

impl LeaseBook {
    /// A book with no per-resource lease limit.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(BookInner::default()),
            max_per_resource: None,
            counters: Arc::new(StatisticsCollector::new()),
        })
    }

    /// A book that allows at most `max` leases per resource.
    pub fn with_limit(max: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(BookInner::default()),
            max_per_resource: Some(max),
            counters: Arc::new(StatisticsCollector::new()),
        })
    }

    /// A book whose lease traffic feeds the given statistics collector.
    ///
    /// Used internally so manager-wide statistics can report live leases.
    pub(crate) fn with_counters(counters: Arc<StatisticsCollector>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(BookInner::default()),
            max_per_resource: None,
            counters,
        })
    }

    /// Acquire a lease on `id`.
    pub fn acquire(self: &Arc<Self>, id: &ResourceId) -> Result<Lease, ConcurrencyError> {
        let mut inner = self.inner.lock().map_err(|_| ConcurrencyError::Poisoned)?;
        if inner.sealed.contains(id) {
            #[cfg(feature = "tracing")]
            tracing::debug!(id = %id, "lease refused: resource destruction is in progress");
            return Err(ConcurrencyError::Destroying(id.clone()));
        }
        let count = inner.counts.entry(id.clone()).or_insert(0);
        if let Some(max) = self.max_per_resource {
            if *count >= max {
                #[cfg(feature = "tracing")]
                tracing::debug!(id = %id, limit = max, "lease refused: per-resource lease limit reached");
                return Err(ConcurrencyError::LeaseLimit(id.clone()));
            }
        }
        *count += 1;
        // the book outlives every lease it issues, so this is the one
        // authoritative place to count from
        self.counters.record_lease_acquire();
        metrics_support::active_leases_change(1.0);
        #[cfg(feature = "tracing")]
        tracing::debug!(id = %id, "lease acquired");
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
            .counts
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
            .counts
            .values()
            .sum())
    }

    /// Try to seal `id` for destruction.
    ///
    /// Returns true when the seal went in, meaning no leases were held
    /// and no new lease may be acquired until [`Self::unseal`] runs.
    /// Returns false when leases are held; the resource stays untouched.
    pub(crate) fn try_seal(&self, id: &ResourceId) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            // a poisoned book must not let destruction through
            return false;
        };
        if inner.counts.get(id).copied().unwrap_or(0) > 0 {
            return false;
        }
        inner.sealed.insert(id.clone());
        true
    }

    /// Release the destruction seal on `id`.
    pub(crate) fn unseal(&self, id: &ResourceId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sealed.remove(id);
            // a zero-count entry has no information value, drop it
            if inner.counts.get(id).copied().unwrap_or(0) == 0 {
                inner.counts.remove(id);
            }
        }
    }

    fn release(&self, id: &ResourceId) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(count) = inner.counts.get_mut(id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    inner.counts.remove(id);
                }
            }
        }
        self.counters.record_lease_release();
        metrics_support::active_leases_change(-1.0);
        #[cfg(feature = "tracing")]
        tracing::debug!(id = %id, "lease released");
    }
}

impl Default for LeaseBook {
    fn default() -> Self {
        Self {
            inner: Mutex::new(BookInner::default()),
            max_per_resource: None,
            counters: Arc::new(StatisticsCollector::new()),
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

    #[test]
    fn sealed_resource_refuses_new_leases() {
        let book = LeaseBook::new();
        let id = ResourceId::new();
        assert!(book.try_seal(&id));
        assert!(matches!(
            book.acquire(&id),
            Err(ConcurrencyError::Destroying(_))
        ));
        book.unseal(&id);
        assert!(book.acquire(&id).is_ok());
    }

    #[test]
    fn seal_fails_while_leases_are_held() {
        let book = LeaseBook::new();
        let id = ResourceId::new();
        let lease = book.acquire(&id).unwrap();
        assert!(!book.try_seal(&id));
        drop(lease);
        assert!(book.try_seal(&id));
    }

    #[test]
    fn lease_release_survives_unwind() {
        let book = LeaseBook::new();
        let id = ResourceId::new();
        let book2 = Arc::clone(&book);
        let id2 = id.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _lease = book2.acquire(&id2).unwrap();
            panic!("simulated failure while holding a lease");
        }));
        assert!(result.is_err());
        assert_eq!(book.lease_count(&id).unwrap(), 0);
    }

    #[test]
    fn standalone_book_tracks_its_own_total() {
        let book = LeaseBook::new();
        let a = ResourceId::new();
        let b = ResourceId::new();
        let _l1 = book.acquire(&a).unwrap();
        let _l2 = book.acquire(&a).unwrap();
        let _l3 = book.acquire(&b).unwrap();
        assert_eq!(book.total_leases().unwrap(), 3);
    }
}
