//! A runtime-scoped resource: lifetime bound to one operation or session.
//!
//! Carries no payload of its own. It exists so resources that belong
//! to a connection, request or other short-lived unit can be tracked,
//! quota-counted and cleaned up through the same registry as files
//! and buffers.

use std::sync::Arc;

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;
use crate::ownership::{Lease, OwnershipCore};

/// A registered runtime-scoped marker.
#[derive(Clone, Debug)]
pub struct RuntimeResource {
    id: ResourceId,
    name: String,
    core: Arc<OwnershipCore>,
}

impl RuntimeResource {
    pub(crate) fn new(id: ResourceId, name: String, core: Arc<OwnershipCore>) -> Self {
        Self { id, name, core }
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

    /// Name the resource was registered under.
    pub fn name(&self) -> &str {
        &self.name
    }
}
