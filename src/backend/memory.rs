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
        self.inner.lock().map(|m| m.values().sum()).unwrap_or(0)
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
        let current: u64 = map.values().sum();
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
}
