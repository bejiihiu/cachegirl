//! A reusable byte buffer with explicit capacity semantics.
//!
//! Wraps `Vec<u8>` and keeps the allocation between uses so hot paths
//! can recycle buffers instead of churning the allocator.

use crate::core::resource::ResourceId;

/// An owned, reusable byte buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Buffer {
    data: Vec<u8>,
}

impl Buffer {
    /// Allocate a buffer with the given capacity, empty length.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
        }
    }

    /// Wrap an existing vec.
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Number of bytes currently held.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when no bytes are held.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Allocated capacity.
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Borrow the contents.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Borrow the contents mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Append bytes, growing as needed.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }

    /// Forget the contents but keep the allocation for reuse.
    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Consume the buffer and return the inner vec.
    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }
}

impl AsRef<[u8]> for Buffer {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// A buffer registered in the registry.
///
/// Holds the actual allocation; dropping it frees the memory. The
/// registry entry follows the namespace's cleanup rules, not the
/// handle's destructor.
#[derive(Clone, Debug)]
pub struct BufferResource {
    id: ResourceId,
    buffer: Buffer,
}

impl BufferResource {
    /// Wrap a buffer with its resource id.
    pub fn new(id: ResourceId, buffer: Buffer) -> Self {
        Self { id, buffer }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Borrow the contents.
    pub fn as_slice(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// Borrow the contents mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut_slice()
    }

    /// Number of bytes currently held.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// True when no bytes are held.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Allocated capacity.
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Append bytes, growing as needed.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Forget the contents but keep the allocation for reuse.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Consume the handle and return the inner vec.
    pub fn into_vec(self) -> Vec<u8> {
        self.buffer.into_inner()
    }
}

impl AsRef<[u8]> for BufferResource {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_keeps_capacity() {
        let mut buf = Buffer::with_capacity(1024);
        buf.extend_from_slice(&[1, 2, 3]);
        assert_eq!(buf.len(), 3);
        let cap = buf.capacity();
        buf.clear();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.capacity(), cap);
    }
}
