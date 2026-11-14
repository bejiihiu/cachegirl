//! A resource materialized by a user-registered backend.
//!
//! The handle is a thin view: the backend owns the physical side, the
//! registry owns the bookkeeping.

use crate::core::resource::ResourceId;

/// A registered resource backed by a custom [`crate::backend::ResourceBackend`].
#[derive(Clone, Debug)]
pub struct CustomResource {
    id: ResourceId,
    backend: String,
}

impl CustomResource {
    /// Wrap a backend name with its resource id.
    pub fn new(id: ResourceId, backend: String) -> Self {
        Self { id, backend }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Name of the backend that materialized the resource.
    pub fn backend(&self) -> &str {
        &self.backend
    }
}
