//! A reusable byte buffer with explicit capacity semantics.
//!
//! Wraps `Vec<u8>` and keeps the allocation between uses so hot paths
//! can recycle buffers instead of churning the allocator.

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
