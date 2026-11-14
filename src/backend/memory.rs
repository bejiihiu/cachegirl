//! Memory backend: tracks byte counts of in-memory resources.
//!
//! It does not own the memory itself — the application keeps the
//! buffers. What it provides is accounting and a release hook so
//! cleanup and quota checks work uniformly across backends.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{ResourceBackend, ResourceRequest};
use crate::core::error::{BackendError, QuotaError, Result};
use crate::core::resource::{ResourceKind, ResourceMeta};

/// Byte accounting for memory-backed resources.
pub struct MemoryBackend {
    max_bytes: Option<u64>,
    inner: Mutex<HashMap<crate::core::resource::ResourceId, u64>>,
}

impl MemoryBackend {
    /// Backend with no overall byte limit.
    pub fn new() -> Self {
        Self {
            max_bytes: None,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Backend with a hard cap on tracked bytes.
    pub fn with_max_bytes(max_bytes: u64) -> Self {
        Self {
            max_bytes: Some(max_bytes),
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Bytes currently tracked.
    pub fn current_bytes(&self) -> u64 {
        self.inner
            .lock()
            .map(|m| crate::core::saturating_bytes(m.values().copied()))
            .unwrap_or(0)
    }

    /// Number of tracked resources.
    pub fn tracked(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceBackend for MemoryBackend {
    fn name(&self) -> &str {
        "memory"
    }

    fn create(&self, request: &ResourceRequest) -> Result<ResourceMeta> {
        if !matches!(
            request.kind,
            ResourceKind::Buffer
                | ResourceKind::Cache
                | ResourceKind::Serialization
                | ResourceKind::Runtime
                | ResourceKind::Custom
        ) {
            return Err(BackendError::Rejected {
                backend: self.name().into(),
                reason: format!("kind `{}` does not live in memory", request.kind),
            }
            .into());
        }
        let size = request.size_hint.unwrap_or(0);
        let mut map = self.inner.lock().map_err(|_| BackendError::Rejected {
            backend: self.name().into(),
            reason: "accounting lock poisoned".into(),
        })?;
        let current: u64 = crate::core::saturating_bytes(map.values().copied());
        if let Some(limit) = self.max_bytes {
            let projected = current.saturating_add(size);
            if projected > limit {
                return Err(QuotaError::SizeExceeded {
                    limit,
                    requested: projected,
                }
                .into());
            }
        }
        let mut meta = ResourceMeta::new(&request.name, &request.namespace, request.kind);
        meta.size_bytes = size;
        for (k, v) in &request.metadata {
            meta.metadata.insert(k.clone(), v.clone());
        }
        map.insert(meta.id.clone(), size);
        Ok(meta)
    }

    fn release(&self, meta: &ResourceMeta) -> Result<()> {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(&meta.id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_and_releases_bytes() {
        let be = MemoryBackend::with_max_bytes(100);
        let req = ResourceRequest::new("buf", "ns", ResourceKind::Buffer).with_size_hint(64);
        let meta = be.create(&req).unwrap();
        assert_eq!(be.current_bytes(), 64);
        be.release(&meta).unwrap();
        assert_eq!(be.current_bytes(), 0);
        assert_eq!(be.tracked(), 0);
    }

    #[test]
    fn limit_is_enforced() {
        let be = MemoryBackend::with_max_bytes(10);
        let req = ResourceRequest::new("buf", "ns", ResourceKind::Buffer).with_size_hint(64);
        assert!(be.create(&req).is_err());
    }

    #[test]
    fn with_max_bytes_zero_allows_only_empty_allocations() {
        let be = MemoryBackend::with_max_bytes(0);
        let empty = ResourceRequest::new("zero", "ns", ResourceKind::Buffer).with_size_hint(0);
        assert!(be.create(&empty).is_ok());
        let one = ResourceRequest::new("one", "ns", ResourceKind::Buffer).with_size_hint(1);
        assert!(matches!(
            be.create(&one),
            Err(crate::core::error::Error::Quota(
                crate::core::error::QuotaError::SizeExceeded { .. }
            ))
        ));
    }

    #[test]
    fn release_of_unknown_id_is_a_noop() {
        let be = MemoryBackend::with_max_bytes(100);
        let meta = ResourceMeta::new("ghost", "ns", ResourceKind::Buffer);
        // release must tolerate records the backend never saw (e.g. after
        // a crash); accounting is keyed by id, so there is nothing to remove
        assert!(be.release(&meta).is_ok());
        assert_eq!(be.current_bytes(), 0);
        assert_eq!(be.tracked(), 0);
    }

    #[test]
    fn room_freed_by_release_can_be_used_again() {
        let be = MemoryBackend::with_max_bytes(64);
        let req = ResourceRequest::new("buf", "ns", ResourceKind::Buffer).with_size_hint(64);
        let meta = be.create(&req).unwrap();
        be.release(&meta).unwrap();
        let again = ResourceRequest::new("buf2", "ns", ResourceKind::Buffer).with_size_hint(64);
        assert!(be.create(&again).is_ok());
    }

    #[test]
    fn create_without_size_hint_accounts_zero_bytes() {
        let be = MemoryBackend::with_max_bytes(0); // zero cap still fits a zero alloc
        let req = ResourceRequest::new("buf", "ns", ResourceKind::Buffer);
        let meta = be.create(&req).unwrap();
        assert_eq!(meta.size_bytes, 0);
        assert_eq!(be.current_bytes(), 0);
        assert_eq!(be.tracked(), 1);
    }

    #[test]
    fn file_kind_is_rejected_with_a_reason() {
        let be = MemoryBackend::new();
        let req = ResourceRequest::new("f.bin", "ns", ResourceKind::File);
        match be.create(&req) {
            Err(crate::core::error::Error::Backend(BackendError::Rejected { reason, .. })) => {
                assert!(reason.contains("file"));
            }
            other => panic!("expected a rejected kind, got {other:?}"),
        }
    }

    #[test]
    fn concurrent_creates_respect_the_limit_exactly() {
        use std::sync::Arc;
        // 10 racers, 5 slots of 100 bytes each: exactly five may win
        let be = Arc::new(MemoryBackend::with_max_bytes(500));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let be = Arc::clone(&be);
            handles.push(std::thread::spawn(move || {
                let req =
                    ResourceRequest::new("buf", "ns", ResourceKind::Buffer).with_size_hint(100);
                be.create(&req)
            }));
        }
        let mut ok = 0usize;
        let mut rejected = 0usize;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => ok += 1,
                Err(crate::core::error::Error::Quota(_)) => rejected += 1,
                Err(other) => panic!("unexpected error: {other}"),
            }
        }
        assert_eq!(ok, 5);
        assert_eq!(rejected, 5);
        assert_eq!(be.current_bytes(), 500);
        assert_eq!(be.tracked(), 5);
    }
}
