//! A cache entry: bytes plus the bookkeeping needed for eviction.

use std::time::SystemTime;

/// One cached value with its access bookkeeping.
#[derive(Clone, Debug)]
pub struct CacheEntry {
    /// Lookup key.
    pub key: String,
    /// Stored bytes.
    pub value: Vec<u8>,
    /// When the entry was created.
    pub created_at: SystemTime,
    /// When the entry was last read.
    pub last_accessed: SystemTime,
}

impl CacheEntry {
    /// Insert a fresh entry.
    pub fn new(key: impl Into<String>, value: Vec<u8>) -> Self {
        let now = SystemTime::now();
        Self {
            key: key.into(),
            value,
            created_at: now,
            last_accessed: now,
        }
    }

    /// Size of the stored payload.
    pub fn len(&self) -> usize {
        self.value.len()
    }

    /// True when the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Mark the entry as used right now.
    pub fn touch(&mut self) {
        self.last_accessed = SystemTime::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_updates_last_access() {
        let mut entry = CacheEntry::new("k", vec![1, 2, 3]);
        let before = entry.last_accessed;
        std::thread::sleep(std::time::Duration::from_millis(2));
        entry.touch();
        assert!(entry.last_accessed >= before);
        assert_eq!(entry.len(), 3);
    }
}
