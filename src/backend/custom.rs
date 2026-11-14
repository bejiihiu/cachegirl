//! Registry for user-provided backends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::ResourceBackend;

/// Name-indexed table of backends, including custom ones.
///
/// This is how applications plug their own storage into the manager
/// without modifying the crate.
#[derive(Default)]
pub struct BackendRegistry {
    map: Mutex<HashMap<String, Arc<dyn ResourceBackend>>>,
}

impl BackendRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a backend under its own name. Replaces any existing entry.
    pub fn register(
        &self,
        backend: Arc<dyn ResourceBackend>,
    ) -> Result<(), crate::core::error::ConcurrencyError> {
        let mut map = self
            .map
            .lock()
            .map_err(|_| crate::core::error::ConcurrencyError::Poisoned)?;
        map.insert(backend.name().to_owned(), backend);
        Ok(())
    }

    /// Look a backend up by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ResourceBackend>> {
        self.map.lock().ok().and_then(|map| map.get(name).cloned())
    }

    /// Names of all registered backends.
    pub fn names(&self) -> Vec<String> {
        self.map
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// How many backends are registered.
    pub fn len(&self) -> usize {
        self.map.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// True when no backends are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ResourceRequest;
    use crate::core::error::Result;
    use crate::core::resource::ResourceMeta;

    struct Dummy;
    impl ResourceBackend for Dummy {
        fn name(&self) -> &str {
            "dummy"
        }

        fn create(&self, _request: &ResourceRequest) -> Result<ResourceMeta> {
            Ok(ResourceMeta::new(
                "x",
                "ns",
                crate::core::resource::ResourceKind::Custom,
            ))
        }

        fn release(&self, _meta: &ResourceMeta) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn register_and_lookup() {
        let reg = BackendRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(Dummy)).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.names(), vec!["dummy".to_string()]);
        assert!(reg.get("dummy").is_some());
        assert!(reg.get("nope").is_none());
    }
}
