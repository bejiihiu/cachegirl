//! A runtime-scoped resource: lifetime bound to one operation or session.
//!
//! Carries no payload of its own. It exists so resources that belong
//! to a connection, request or other short-lived unit can be tracked,
//! quota-counted and cleaned up through the same registry as files
//! and buffers.

use crate::core::resource::ResourceId;

/// A registered runtime-scoped marker.
#[derive(Clone, Debug)]
pub struct RuntimeResource {
    id: ResourceId,
    name: String,
}

impl RuntimeResource {
    /// Wrap a name with its resource id.
    pub fn new(id: ResourceId, name: String) -> Self {
        Self { id, name }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Name the resource was registered under.
    pub fn name(&self) -> &str {
        &self.name
    }
}
