//! Namespaces: logical isolation for resources.
//!
//! Every resource lives in exactly one namespace. Namespaces carry
//! their own quota and metadata, and can never touch resources that
//! belong to a different namespace through this API.

mod create;
mod quota;
mod shared;

pub use create::CreateOptions;
pub use quota::NamespaceQuota;
#[cfg(test)]
pub(crate) use shared::test_shared;
pub(crate) use shared::{Backends, NamespaceShared};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::core::error::{AccessError, ConfigError};
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

/// Validate a resource name: non-empty, no traversal, no control characters.
pub(crate) fn validate_resource_name(name: &str) -> Result<(), AccessError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
    {
        return Err(AccessError::UnsafeName(name.to_owned()));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(AccessError::UnsafeName(name.to_owned()));
    }
    Ok(())
}

/// A logical isolation boundary for resources.
///
/// Cheap to clone; all clones point at the same underlying state. A
/// namespace shares its manager's registry, backends and quota
/// plumbing through an internal reference created at `TempManager`
/// build time.
pub struct Namespace {
    name: String,
    registry: Arc<Registry>,
    shared: Arc<NamespaceShared>,
    quota: RwLock<NamespaceQuota>,
    metadata: Mutex<HashMap<String, String>>,
    cleanup_policy: RwLock<Option<crate::policy::CleanupPolicy>>,
    custom_cleanup_policy: RwLock<Option<Arc<dyn crate::policy::CustomCleanupPolicy>>>,
}

impl Namespace {
    pub(crate) fn new(name: String, registry: Arc<Registry>, shared: Arc<NamespaceShared>) -> Self {
        Self {
            name,
            registry,
            shared,
            quota: RwLock::new(NamespaceQuota::unlimited()),
            metadata: Mutex::new(HashMap::new()),
            cleanup_policy: RwLock::new(None),
            custom_cleanup_policy: RwLock::new(None),
        }
    }

    /// Access to the shared manager plumbing (registry, backends, sink).
    pub(crate) fn shared(&self) -> &Arc<NamespaceShared> {
        &self.shared
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

    /// Effective cleanup policy of this namespace.
    ///
    /// Returns the namespace-specific policy if one is set, otherwise
    /// the manager-wide policy, otherwise an unlimited default.
    pub fn cleanup_policy(&self) -> crate::policy::CleanupPolicy {
        self.cleanup_policy
            .read()
            .ok()
            .and_then(|guard| *guard)
            .or_else(|| {
                self.shared
                    .cleanup_policy
                    .read()
                    .ok()
                    .and_then(|guard| *guard)
            })
            .unwrap_or_default()
    }

    /// Set or replace the cleanup policy for this namespace.
    ///
    /// The policy governs the next cleanup passes: which resources
    /// expire, aggregate quota eviction within this namespace, and the
    /// default eviction priority for newly created resources.
    pub fn set_cleanup_policy(&self, policy: crate::policy::CleanupPolicy) {
        if let Ok(mut guard) = self.cleanup_policy.write() {
            *guard = Some(policy);
        }
    }

    /// Remove the namespace-specific policy; the namespace falls back
    /// to the manager-wide policy (or no policy) again.
    pub fn clear_cleanup_policy(&self) {
        if let Ok(mut guard) = self.cleanup_policy.write() {
            *guard = None;
        }
    }

    /// Set a custom cleanup policy for this namespace.
    ///
    /// The policy is consulted for resources that are not otherwise
    /// eligible for cleanup; it overrides the manager-wide custom
    /// policy when both are set. Leases still win over a removal verdict.
    pub fn set_custom_cleanup_policy(
        &self,
        policy: std::sync::Arc<dyn crate::policy::CustomCleanupPolicy>,
    ) {
        if let Ok(mut guard) = self.custom_cleanup_policy.write() {
            *guard = Some(policy);
        }
    }

    /// Remove the namespace-specific custom policy, if any.
    pub fn clear_custom_cleanup_policy(&self) {
        if let Ok(mut guard) = self.custom_cleanup_policy.write() {
            *guard = None;
        }
    }

    /// Effective custom cleanup policy: namespace-specific if set,
    /// otherwise the manager-wide one.
    pub(crate) fn effective_custom_cleanup_policy(
        &self,
    ) -> Option<Arc<dyn crate::policy::CustomCleanupPolicy>> {
        self.custom_cleanup_policy
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .or_else(|| {
                self.shared
                    .custom_cleanup_policy
                    .read()
                    .ok()
                    .and_then(|guard| guard.clone())
            })
    }

    /// Reclaim resources for this namespace and one scope only.
    ///
    /// Equivalent to [`crate::TempManager::cleanup_expired`] but scoped to
    /// this namespace: other namespaces are neither scanned nor evicted,
    /// and manager-wide limits are not applied. Reports carry the same
    /// breakdown as manager-wide passes.
    pub fn cleanup_expired(&self) -> crate::core::error::Result<crate::cleanup::CleanupReport> {
        crate::cleanup::CleanupEngine::cleanup_namespace(
            &self.shared,
            &self.name,
            self.sweep_view(),
        )
    }

    /// Crate-internal view for the cleanup engine.
    pub(crate) fn sweep_view(&self) -> crate::cleanup::SweepView {
        crate::cleanup::SweepView {
            policy: self.cleanup_policy(),
            custom: self.effective_custom_cleanup_policy(),
            quota: self.quota(),
        }
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
    fn resource_names_are_validated() {
        assert!(validate_resource_name("packet-dump.bin").is_ok());
        assert!(validate_resource_name("").is_err());
        assert!(validate_resource_name(" padded").is_err());
        assert!(validate_resource_name(".").is_err());
        assert!(validate_resource_name("..").is_err());
        assert!(validate_resource_name("a/b").is_err());
        assert!(validate_resource_name("a\\b").is_err());
        assert!(validate_resource_name("a\u{7}b").is_err());
    }

    #[test]
    fn quota_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let shared = test_shared(dir.path());
        let ns = Namespace::new("test".into(), Arc::clone(&shared.registry), shared);
        assert_eq!(ns.quota(), NamespaceQuota::unlimited());
        ns.set_quota(NamespaceQuota::with_max_bytes(1024));
        assert_eq!(ns.quota().max_bytes, Some(1024));
    }
}
