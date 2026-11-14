//! A serialization intermediate registered in the registry.
//!
//! Used for (de)serialization payloads that exist for the duration of
//! one pass and then go away. Owns the bytes; what happens on drop
//! depends on the ownership mode the resource was created with.

use std::sync::Arc;

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;
use crate::ownership::{Lease, OwnershipCore};

/// A registered serialization intermediate.
#[derive(Clone, Debug)]
pub struct SerializationResource {
    id: ResourceId,
    bytes: Vec<u8>,
    core: Arc<OwnershipCore>,
}

impl SerializationResource {
    pub(crate) fn new(id: ResourceId, bytes: Vec<u8>, core: Arc<OwnershipCore>) -> Self {
        Self { id, bytes, core }
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

    /// Borrow the bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Number of bytes held.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True when no bytes are held.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Consume the handle and return the inner vec.
    ///
    /// Consuming the handle drops it; the ownership mode applies to it
    /// like to any other drop.
    pub fn into_vec(mut self) -> Vec<u8> {
        core::mem::take(&mut self.bytes)
    }
}

impl AsRef<[u8]> for SerializationResource {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}
