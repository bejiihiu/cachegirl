//! A resource materialized by a user-registered backend.
//!
//! The handle is a thin view: the backend owns the physical side, the
//! registry owns the bookkeeping.

use std::sync::Arc;

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;
use crate::ownership::{Lease, OwnershipCore};

/// A registered resource backed by a custom [`crate::backend::ResourceBackend`].
#[derive(Clone, Debug)]
pub struct CustomResource {
    id: ResourceId,
    backend: String,
    core: Arc<OwnershipCore>,
}

impl CustomResource {
    pub(crate) fn new(id: ResourceId, backend: String, core: Arc<OwnershipCore>) -> Self {
        Self { id, backend, core }
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

    /// Name of the backend that materialized the resource.
    pub fn backend(&self) -> &str {
        &self.backend
    }
}
