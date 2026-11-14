//! In-memory registry of managed resources.
//!
//! The registry is the single source of truth about what exists, who
//! owns it, and which lifecycle state it is in. It is thread-safe and
//! deliberately free of any I/O so it stays cheap to use anywhere.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::error::RegistryError;
use crate::core::lifecycle::ResourceState;
use crate::core::resource::{ResourceId, ResourceMeta};

/// Shared, thread-safe registry of [`ResourceMeta`] records.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<HashMap<ResourceId, ResourceMeta>>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new resource. Fails if the id is already present.
    pub fn register(&self, meta: ResourceMeta) -> Result<(), RegistryError> {
        let mut map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        if map.contains_key(&meta.id) {
            return Err(RegistryError::AlreadyRegistered(meta.id));
        }
        map.insert(meta.id.clone(), meta);
        Ok(())
    }

    /// Fetch a snapshot of a resource's metadata.
    pub fn get(&self, id: &ResourceId) -> Result<ResourceMeta, RegistryError> {
        let map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        map.get(id)
            .cloned()
            .ok_or_else(|| RegistryError::NotFound(id.clone()))
    }

    /// Apply a mutation to a registered resource.
    pub fn update(
        &self,
        id: &ResourceId,
        f: impl FnOnce(&mut ResourceMeta),
    ) -> Result<ResourceMeta, RegistryError> {
        let mut map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let meta = map
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
        f(meta);
        Ok(meta.clone())
    }

    /// Move a resource to a new lifecycle state, validating the transition.
    pub fn set_state(
        &self,
        id: &ResourceId,
        next: ResourceState,
    ) -> Result<ResourceState, RegistryError> {
        let mut map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let meta = map
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
        let from = meta.state;
        let to = from.try_transition(next)?;
        meta.state = to;
        Ok(to)
    }

    /// Remove a resource from the registry and return its last record.
    pub fn remove(&self, id: &ResourceId) -> Result<ResourceMeta, RegistryError> {
        let mut map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        map.remove(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))
    }

    /// Number of registered resources.
    pub fn len(&self) -> Result<usize, RegistryError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .len())
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> Result<bool, RegistryError> {
        Ok(self.len()? == 0)
    }

    /// Snapshot every registered resource.
    pub fn snapshot(&self) -> Result<Vec<ResourceMeta>, RegistryError> {
        let map = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        Ok(map.values().cloned().collect())
    }

    /// Snapshot resources in a single namespace.
    pub fn snapshot_namespace(&self, namespace: &str) -> Result<Vec<ResourceMeta>, RegistryError> {
        Ok(self
            .snapshot()?
            .into_iter()
            .filter(|m| m.namespace == namespace)
            .collect())
    }

    /// Total tracked size across all resources, in bytes.
    pub fn total_bytes(&self) -> Result<u64, RegistryError> {
        Ok(self.snapshot()?.iter().map(|m| m.size_bytes).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::ResourceKind;

    fn meta(ns: &str) -> ResourceMeta {
        ResourceMeta::new("test", ns, ResourceKind::File)
    }

    #[test]
    fn register_get_remove_roundtrip() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        assert_eq!(reg.len().unwrap(), 1);
        let got = reg.get(&id).unwrap();
        assert_eq!(got.namespace, "a");
        let removed = reg.remove(&id).unwrap();
        assert_eq!(removed.id, id);
        assert!(reg.is_empty().unwrap());
    }

    #[test]
    fn removing_unknown_resource_fails() {
        let reg = Registry::new();
        let id = ResourceId::new();
        assert!(matches!(reg.remove(&id), Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn invalid_state_transition_keeps_old_state() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        assert!(reg.set_state(&id, ResourceState::Destroyed).is_err());
        let got = reg.get(&id).unwrap();
        assert_eq!(got.state, ResourceState::Created);
    }

    #[test]
    fn registering_the_same_id_twice_is_rejected() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let mut dup = meta("b");
        dup.id = id.clone();
        assert!(matches!(
            reg.register(dup),
            Err(RegistryError::AlreadyRegistered(_))
        ));
        // the original record must survive the rejected registration
        assert_eq!(reg.get(&id).unwrap().namespace, "a");
        assert_eq!(reg.len().unwrap(), 1);
    }

    #[test]
    fn missing_ids_surface_not_found_everywhere() {
        let reg = Registry::new();
        let id = ResourceId::new();
        assert!(matches!(reg.get(&id), Err(RegistryError::NotFound(_))));
        assert!(matches!(reg.remove(&id), Err(RegistryError::NotFound(_))));
        assert!(matches!(
            reg.update(&id, |m| m.size_bytes = 1),
            Err(RegistryError::NotFound(_))
        ));
        assert!(matches!(
            reg.set_state(&id, ResourceState::Active),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn update_applies_the_mutation_and_returns_the_new_snapshot() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let updated = reg.update(&id, |m| m.size_bytes += 7).unwrap();
        assert_eq!(updated.size_bytes, 7);
        assert_eq!(reg.get(&id).unwrap().size_bytes, 7);
    }

    #[test]
    fn snapshot_hands_out_copies_not_aliases() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let mut got = reg.snapshot().unwrap();
        got[0].name = "tampered".into();
        // the registry must not observe mutations on handed-out records
        assert_eq!(reg.get(&id).unwrap().name, "test");
    }

    #[test]
    fn snapshot_namespace_filters_and_total_bytes_sums() {
        let reg = Registry::new();
        for (ns, bytes) in [("a", 10), ("a", 15), ("b", 100)] {
            let mut m = meta(ns);
            m.size_bytes = bytes;
            reg.register(m).unwrap();
        }
        let a = reg.snapshot_namespace("a").unwrap();
        assert_eq!(a.len(), 2);
        let b = reg.snapshot_namespace("b").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(reg.total_bytes().unwrap(), 125);
    }

    #[test]
    fn panicking_update_poisons_the_registry() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        // a panicking closure while the registry lock is held poisons it,
        // and every operation after that must fail with Poisoned
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reg.update(&id, |_| panic!("boom while holding the lock"));
        }));
        assert!(matches!(reg.get(&id), Err(RegistryError::Poisoned)));
        assert!(matches!(
            reg.register(meta("b")),
            Err(RegistryError::Poisoned)
        ));
        assert!(matches!(reg.len(), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.remove(&id), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.snapshot(), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.total_bytes(), Err(RegistryError::Poisoned)));
    }

    #[test]
    fn concurrent_registrations_and_increments_land_exactly_once() {
        use std::sync::Arc;
        let reg = Arc::new(Registry::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    reg.register(meta("shared")).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.len().unwrap(), 200);

        // concurrent increments through the mutation path must not lose updates
        let target = reg.snapshot().unwrap().into_iter().next().unwrap().id;
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    reg.update(&target, |m| m.size_bytes += 1).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.get(&target).unwrap().size_bytes, 200);
    }
}
