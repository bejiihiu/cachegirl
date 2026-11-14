//! Namespaces: logical isolation for resources.
//!
//! Every resource lives in exactly one namespace. Namespaces carry
//! their own quota and metadata, and can never touch resources that
//! belong to a different namespace through this API.

mod quota;

pub use quota::NamespaceQuota;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::core::error::ConfigError;
use crate::core::registry::Registry;
use crate::core::resource::ResourceMeta;

/// Validate a namespace name: non-empty, no path traversal, sane charset.
pub(crate) fn validate_namespace_name(name: &str) -> Result<(), ConfigError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed != name {
        return Err(ConfigError::InvalidNamespaceName(name.to_owned()));
    }
    if name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(ConfigError::InvalidNamespaceName(name.to_owned()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ConfigError::InvalidNamespaceName(name.to_owned()));
    }
    Ok(())
}

/// A logical isolation boundary for resources.
///
/// Cheap to clone; all clones point at the same underlying state.
pub struct Namespace {
    name: String,
    registry: Arc<Registry>,
    quota: RwLock<NamespaceQuota>,
    metadata: Mutex<HashMap<String, String>>,
}

impl Namespace {
    pub(crate) fn new(name: String, registry: Arc<Registry>) -> Self {
        Self {
            name,
            registry,
            quota: RwLock::new(NamespaceQuota::unlimited()),
            metadata: Mutex::new(HashMap::new()),
        }
    }

    /// Namespace name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Current quota for this namespace.
    pub fn quota(&self) -> NamespaceQuota {
        self.quota
            .read()
            .map(|q| *q)
            .unwrap_or(NamespaceQuota::unlimited())
    }

    /// Replace the quota for this namespace.
    pub fn set_quota(&self, quota: NamespaceQuota) {
        if let Ok(mut q) = self.quota.write() {
            *q = quota;
        }
    }

    /// Current usage: bytes and live resource count.
    pub fn usage(&self) -> crate::core::error::Result<(u64, u64)> {
        let items = self.registry.snapshot_namespace(&self.name)?;
        let bytes = items.iter().map(|m| m.size_bytes).sum();
        Ok((bytes, items.len() as u64))
    }

    /// Snapshot of the resources currently registered in this namespace.
    pub fn resources(&self) -> crate::core::error::Result<Vec<ResourceMeta>> {
        self.registry
            .snapshot_namespace(&self.name)
            .map_err(Into::into)
    }

    /// Attach a free-form metadata entry to the namespace.
    pub fn set_metadata(&self, key: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut map) = self.metadata.lock() {
            map.insert(key.into(), value.into());
        }
    }

    /// Read a metadata entry.
    pub fn metadata(&self, key: &str) -> Option<String> {
        self.metadata.lock().ok().and_then(|m| m.get(key).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        assert!(validate_namespace_name("network").is_ok());
        assert!(validate_namespace_name("packets-v2_01.bin").is_ok());
        assert!(validate_namespace_name("").is_err());
        assert!(validate_namespace_name("../etc").is_err());
        assert!(validate_namespace_name("a/b").is_err());
        assert!(validate_namespace_name("a\\b").is_err());
        assert!(validate_namespace_name("a b").is_err());
    }

    #[test]
    fn quota_roundtrip() {
        let reg = Arc::new(Registry::new());
        let ns = Namespace::new("test".into(), reg);
        assert_eq!(ns.quota(), NamespaceQuota::unlimited());
        ns.set_quota(NamespaceQuota::with_max_bytes(1024));
        assert_eq!(ns.quota().max_bytes, Some(1024));
    }
}
