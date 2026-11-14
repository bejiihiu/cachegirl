//! A serialization intermediate registered in the registry.
//!
//! Used for (de)serialization payloads that exist for the duration of
//! one pass and then go away. Owns the bytes; the registry entry
//! follows the namespace's cleanup rules.

use crate::core::resource::ResourceId;

/// A registered serialization intermediate.
#[derive(Clone, Debug)]
pub struct SerializationResource {
    id: ResourceId,
    bytes: Vec<u8>,
}

impl SerializationResource {
    /// Wrap bytes with their resource id.
    pub fn new(id: ResourceId, bytes: Vec<u8>) -> Self {
        Self { id, bytes }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
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
    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

impl AsRef<[u8]> for SerializationResource {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}
