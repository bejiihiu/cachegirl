//! RAII handle tying a resource id to registry bookkeeping.
//!
//! Dropping the handle unregisters the resource. Physical deletion
//! remains the backend's job — Drop never touches the filesystem.

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::registry::Registry;
use crate::core::resource::{ResourceId, ResourceMeta};

/// Owning handle to a registered resource.
pub struct ResourceHandle {
    id: ResourceId,
    registry: Arc<Registry>,
}

impl ResourceHandle {
    /// Register a handle for `id` against `registry`.
    pub fn new(id: ResourceId, registry: Arc<Registry>) -> Self {
        Self { id, registry }
    }

    /// Id of the underlying resource.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Snapshot of the current registry record.
    pub fn meta(&self) -> Result<ResourceMeta> {
        self.registry.get(&self.id).map_err(Into::into)
    }

    /// Hand the id out without unregistering; the handle keeps ownership.
    pub fn detach(self) -> ResourceId {
        let id = self.id.clone();
        std::mem::forget(self);
        id
    }
}

impl Drop for ResourceHandle {
    fn drop(&mut self) {
        // unregistering must never panic inside a destructor
        let _ = self.registry.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::ResourceKind;

    #[test]
    fn drop_unregisters() {
        let reg = Arc::new(Registry::new());
        let meta = ResourceMeta::new("x", "ns", ResourceKind::File);
        let id = meta.id.clone();
        reg.register(meta).unwrap();
        {
            let _handle = ResourceHandle::new(id.clone(), reg.clone());
            assert_eq!(reg.len().unwrap(), 1);
        }
        assert_eq!(reg.len().unwrap(), 0);
    }

    #[test]
    fn detach_keeps_registration() {
        let reg = Arc::new(Registry::new());
        let meta = ResourceMeta::new("x", "ns", ResourceKind::File);
        let id = meta.id.clone();
        reg.register(meta).unwrap();
        let leaked = ResourceHandle::new(id, reg.clone()).detach();
        assert_eq!(reg.len().unwrap(), 1);
        let _ = reg.remove(&leaked);
    }
}
