//! A cache entry: bytes plus the bookkeeping needed for eviction.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;
use crate::ownership::{Lease, OwnershipCore};

/// One cached value with its access bookkeeping.
///
/// `created_at` and `last_accessed` serialize as
/// `{"secs_since_epoch": ..., "nanos_since_epoch": ...}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CacheEntry {
    /// Lookup key.
    pub key: String,
    /// Stored bytes.
    pub value: Vec<u8>,
    /// When the entry was created.
    pub created_at: SystemTime,
    /// When the entry was last read.
    pub last_accessed: SystemTime,
}

impl CacheEntry {
    /// Insert a fresh entry.
    pub fn new(key: impl Into<String>, value: Vec<u8>) -> Self {
        let now = SystemTime::now();
        Self {
            key: key.into(),
            value,
            created_at: now,
            last_accessed: now,
        }
    }

    /// Size of the stored payload.
    pub fn len(&self) -> usize {
        self.value.len()
    }

    /// True when the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Mark the entry as used right now.
    pub fn touch(&mut self) {
        self.last_accessed = SystemTime::now();
    }
}

/// Namespace-local store for live cache entries.
///
/// Entries live behind shared `Arc`s so handles handed out to callers
/// and the store itself point at the same bookkeeping. Keyed by
/// `(namespace, key)`.
/// Shared slot holding one cache entry's bookkeeping.
pub(crate) type SharedEntry = Arc<Mutex<CacheEntry>>;
/// Map key: which namespace, which key.
pub(crate) type CacheKey = (String, String);

/// A store slot: which registry id, and the shared entry itself.
#[derive(Clone)]
pub(crate) struct StoredCacheEntry {
    /// Registry id the entry is filed under.
    pub(crate) id: ResourceId,
    /// The shared entry.
    pub(crate) entry: SharedEntry,
}

#[derive(Default)]
pub(crate) struct CacheStore {
    entries: Mutex<std::collections::HashMap<CacheKey, StoredCacheEntry>>,
}

impl CacheStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Insert only if the key is free; atomically decides the verdict.
    ///
    /// Returns true when the entry was stored, false when the key was
    /// already taken. The check and the excitation happen under one
    /// lock hold, so two concurrent creators of the same key cannot
    /// both win.
    pub(crate) fn insert_if_absent(
        &self,
        namespace: &str,
        key: &str,
        id: ResourceId,
        entry: SharedEntry,
    ) -> bool {
        use std::collections::hash_map::Entry;
        self.entries
            .lock()
            .map(
                |mut m| match m.entry((namespace.to_owned(), key.to_owned())) {
                    Entry::Occupied(_) => false,
                    Entry::Vacant(slot) => {
                        slot.insert(StoredCacheEntry { id, entry });
                        true
                    }
                },
            )
            .unwrap_or(false)
    }

    pub(crate) fn get(&self, namespace: &str, key: &str) -> Option<StoredCacheEntry> {
        self.entries
            .lock()
            .ok()
            .and_then(|m| m.get(&(namespace.to_owned(), key.to_owned())).cloned())
    }

    pub(crate) fn remove(&self, namespace: &str, key: &str) -> Option<StoredCacheEntry> {
        self.entries
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&(namespace.to_owned(), key.to_owned())))
    }
}

/// Handle to a cache entry registered in the registry.
///
/// The bytes are owned by the entry itself. What happens on drop
/// depends on the ownership mode the entry was created with: `Managed`
/// and `Leased` handles detach, `Owned` and `Shared` handles release
/// the entry from the store. Destruction stands down while leases are
/// held.
#[derive(Clone)]
pub struct CacheEntryResource {
    id: ResourceId,
    key: String,
    entry: SharedEntry,
    core: Arc<OwnershipCore>,
}

impl CacheEntryResource {
    pub(crate) fn new(
        id: ResourceId,
        key: String,
        entry: SharedEntry,
        core: Arc<OwnershipCore>,
    ) -> Self {
        Self {
            id,
            key,
            entry,
            core,
        }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Acquire a lease on the resource.
    ///
    /// While at least one lease is held, the resource cannot be
    /// physically released: explicit release and cleanup passes move it
    /// to `PendingCleanup` instead. Dropping the lease releases it.
    pub fn acquire(&self) -> Result<Lease, ConcurrencyError> {
        self.core.acquire_lease()
    }

    /// Lookup key of the entry.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Size of the stored payload.
    pub fn len(&self) -> usize {
        self.entry.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// True when the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clone of the stored payload, if the entry lock is not poisoned.
    pub fn value(&self) -> Result<Vec<u8>, ConcurrencyError> {
        Ok(self
            .entry
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?
            .value
            .clone())
    }

    /// Access the entry in place without cloning the payload.
    pub fn with<R>(&self, f: impl FnOnce(&CacheEntry) -> R) -> Result<R, ConcurrencyError> {
        let guard = self.entry.lock().map_err(|_| ConcurrencyError::Poisoned)?;
        Ok(f(&guard))
    }

    /// Mark the entry as used right now.
    pub fn touch(&self) -> Result<(), ConcurrencyError> {
        self.entry
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?
            .touch();
        Ok(())
    }

    /// When the entry was last read or written.
    pub fn last_accessed(&self) -> Result<SystemTime, ConcurrencyError> {
        Ok(self
            .entry
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?
            .last_accessed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_updates_last_access() {
        let mut entry = CacheEntry::new("k", vec![1, 2, 3]);
        let before = entry.last_accessed;
        std::thread::sleep(core::time::Duration::from_millis(2));
        entry.touch();
        assert!(entry.last_accessed >= before);
        assert_eq!(entry.len(), 3);
    }

    #[test]
    fn new_entries_start_with_both_timestamps_set() {
        let before = SystemTime::now();
        let entry = CacheEntry::new("k", vec![1]);
        assert!(entry.created_at >= before);
        assert_eq!(entry.created_at, entry.last_accessed);
        assert!(!entry.is_empty());
        assert_eq!(entry.len(), 1);
        assert!(CacheEntry::new("empty", Vec::new()).is_empty());
    }

    #[test]
    fn store_is_scoped_by_namespace() {
        let store = CacheStore::new();
        let id = ResourceId::new();
        let entry = Arc::new(Mutex::new(CacheEntry::new("k", vec![1])));
        assert!(store.insert_if_absent("ns-a", "k", id.clone(), Arc::clone(&entry)));
        assert!(!store.insert_if_absent("ns-a", "k", id.clone(), Arc::clone(&entry)));
        assert!(store.get("ns-a", "k").is_some());
        assert!(store.get("ns-b", "k").is_none());
        assert!(store.remove("ns-b", "k").is_none());
        assert!(store.get("ns-a", "k").is_some());
    }

    #[test]
    fn insert_if_absent_reports_a_taken_key_and_changes_nothing() {
        let store = CacheStore::new();
        let first_id = ResourceId::new();
        let second_id = ResourceId::new();
        let entry = Arc::new(Mutex::new(CacheEntry::new("k", vec![1])));
        assert!(store.insert_if_absent("ns", "k", first_id.clone(), Arc::clone(&entry)));
        assert!(!store.insert_if_absent("ns", "k", second_id.clone(), Arc::clone(&entry)));
        // the original slot is untouched by the losing creator
        assert_eq!(store.get("ns", "k").unwrap().id, first_id);
    }

    #[test]
    fn remove_hands_back_the_entry_and_frees_the_key() {
        let store = CacheStore::new();
        let id = ResourceId::new();
        let entry = Arc::new(Mutex::new(CacheEntry::new("k", vec![9, 8])));
        assert!(store.insert_if_absent("ns", "k", id.clone(), entry));
        let removed = store.remove("ns", "k").expect("entry must be removable");
        assert_eq!(removed.id, id);
        assert!(store.get("ns", "k").is_none());
        assert!(store.remove("ns", "k").is_none());
    }
}
