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
}
