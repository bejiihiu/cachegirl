//! Async namespace façade.

use std::sync::Arc;

use crate::CleanupReport;
use crate::core::error::Result;
use crate::core::resource::{ResourceId, ResourceMeta};
use crate::namespace::{CreateOptions, Namespace, NamespaceQuota};

use super::offload;

/// Async handle to a namespace. Clone freely; clones share the same state.
///
/// Operations that can touch the filesystem or a user-registered backend
/// (`create_file`, `namespace creation`, `release`, `refresh_size`,
/// `cleanup_expired`, `shutdown` on the manager) run on Tokio's blocking
/// pool when the `tokio` feature is on and a runtime is ambient.
/// Everything else — buffers, cache entries, registry bookkeeping —
/// stays on the calling thread.
pub struct AsyncNamespace {
    inner: Arc<Namespace>,
}

impl Clone for AsyncNamespace {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl std::fmt::Debug for AsyncNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncNamespace")
            .field("name", &self.inner.name())
            .finish_non_exhaustive()
    }
}

impl AsyncNamespace {
    pub(crate) fn new(inner: Arc<Namespace>) -> Self {
        Self { inner }
    }

    /// Unwrap into the underlying sync namespace.
    pub fn into_inner(self) -> Arc<Namespace> {
        self.inner
    }

    /// Borrow the underlying sync namespace.
    pub fn as_inner(&self) -> &Namespace {
        &self.inner
    }

    /// Namespace name.
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Current quota for this namespace.
    pub async fn quota(&self) -> NamespaceQuota {
        self.inner.quota()
    }

    /// Replace the quota for this namespace.
    pub async fn set_quota(&self, quota: NamespaceQuota) {
        self.inner.set_quota(quota)
    }

    /// Current usage: bytes and live resource count.
    pub async fn usage(&self) -> Result<(u64, u64)> {
        self.inner.usage()
    }

    /// Snapshot of the resources currently registered in this namespace.
    pub async fn resources(&self) -> Result<Vec<ResourceMeta>> {
        self.inner.resources()
    }

    /// Attach a free-form metadata entry to the namespace.
    pub async fn set_metadata(&self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.set_metadata(key, value)
    }

    /// Read a metadata entry.
    pub async fn metadata(&self, key: &str) -> Option<String> {
        self.inner.metadata(key)
    }

    /// Effective cleanup policy of this namespace.
    ///
    /// Returns the namespace-specific policy if one is set, otherwise
    /// the manager-wide policy, otherwise an unlimited default.
    pub async fn cleanup_policy(&self) -> crate::policy::CleanupPolicy {
        self.inner.cleanup_policy()
    }

    /// Set or replace the cleanup policy for this namespace.
    pub async fn set_cleanup_policy(&self, policy: crate::policy::CleanupPolicy) {
        self.inner.set_cleanup_policy(policy)
    }

    /// Remove the namespace-specific policy; the namespace falls back
    /// to the manager-wide policy (or no policy) again.
    pub async fn clear_cleanup_policy(&self) {
        self.inner.clear_cleanup_policy()
    }

    /// Set a custom cleanup policy for this namespace.
    ///
    /// The policy is consulted for resources that are not otherwise
    /// eligible for cleanup; it overrides the manager-wide custom
    /// policy when both are set. Leases still win over a removal verdict.
    pub async fn set_custom_cleanup_policy(
        &self,
        policy: Arc<dyn crate::policy::CustomCleanupPolicy>,
    ) {
        self.inner.set_custom_cleanup_policy(policy)
    }

    /// Remove the namespace-specific custom policy, if any.
    pub async fn clear_custom_cleanup_policy(&self) {
        self.inner.clear_custom_cleanup_policy()
    }

    /// Reclaim resources for this namespace and one scope only.
    ///
    /// Same semantics as the manager-wide pass but scoped: other
    /// namespaces are neither scanned nor evicted. Runs on the blocking
    /// pool under Tokio.
    pub async fn cleanup_expired(&self) -> Result<CleanupReport> {
        let inner = Arc::clone(&self.inner);
        offload(move || inner.cleanup_expired()).await
    }

    // -- filesystem resources ---------------------------------------------

    /// Create an empty file `root/<instance>/<namespace>/<name>`.
    #[cfg(feature = "filesystem")]
    pub async fn create_file(&self, name: &str) -> Result<crate::resource::FileResource> {
        self.create_file_with(name, CreateOptions::new()).await
    }

    /// Create an empty file with owner, metadata and TTL options.
    ///
    /// The file is materialized on disk, so this offloads to the
    /// blocking pool under Tokio.
    #[cfg(feature = "filesystem")]
    pub async fn create_file_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::FileResource> {
        let inner = Arc::clone(&self.inner);
        let name = name.to_owned();
        offload(move || inner.create_file_with(&name, options)).await
    }

    /// Create a directory `root/<instance>/<namespace>/<name>`.
    #[cfg(feature = "filesystem")]
    pub async fn create_directory(&self, name: &str) -> Result<crate::resource::DirectoryResource> {
        self.create_directory_with(name, CreateOptions::new()).await
    }

    /// Create a directory with owner, metadata and TTL options.
    ///
    /// The directory is materialized on disk, so this offloads to the
    /// blocking pool under Tokio.
    #[cfg(feature = "filesystem")]
    pub async fn create_directory_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::DirectoryResource> {
        let inner = Arc::clone(&self.inner);
        let name = name.to_owned();
        offload(move || inner.create_directory_with(&name, options)).await
    }

    // -- in-memory resources ------------------------------------------------

    /// Allocate a buffer of `capacity` bytes.
    ///
    /// Never leaves the calling thread: allocation and quota accounting
    /// are pure memory work.
    #[cfg(feature = "memory")]
    pub async fn create_buffer(&self, capacity: usize) -> Result<crate::resource::BufferResource> {
        self.create_buffer_with(capacity, CreateOptions::new())
            .await
    }

    /// Allocate a buffer with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub async fn create_buffer_with(
        &self,
        capacity: usize,
        options: CreateOptions,
    ) -> Result<crate::resource::BufferResource> {
        self.inner.create_buffer_with(capacity, options)
    }

    /// Store a serialization intermediate registered under `name`.
    #[cfg(feature = "memory")]
    pub async fn create_serialization(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<crate::resource::SerializationResource> {
        self.create_serialization_with(name, bytes, CreateOptions::new())
            .await
    }

    /// Store a serialization intermediate with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub async fn create_serialization_with(
        &self,
        name: &str,
        bytes: Vec<u8>,
        options: CreateOptions,
    ) -> Result<crate::resource::SerializationResource> {
        self.inner.create_serialization_with(name, bytes, options)
    }

    /// Register a runtime-scoped resource tied to the current operation or session.
    ///
    /// Carries no bytes; it exists so lifetime-scoped objects can be
    /// tracked, quota-counted and cleaned up like everything else.
    #[cfg(feature = "memory")]
    pub async fn create_runtime(&self, name: &str) -> Result<crate::resource::RuntimeResource> {
        self.create_runtime_with(name, CreateOptions::new()).await
    }

    /// Register a runtime-scoped resource with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub async fn create_runtime_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::RuntimeResource> {
        self.inner.create_runtime_with(name, options)
    }

    // -- cache resources ------------------------------------------------------

    /// Store a cache entry under `key`.
    ///
    /// Keys are unique within a namespace. The cache store is in-memory,
    /// so this stays on the calling thread.
    #[cfg(feature = "cache")]
    pub async fn create_cache_entry(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> Result<crate::resource::CacheEntryResource> {
        self.create_cache_entry_with(key, value, CreateOptions::new())
            .await
    }

    /// Store a cache entry with owner, metadata and TTL options.
    #[cfg(feature = "cache")]
    pub async fn create_cache_entry_with(
        &self,
        key: &str,
        value: Vec<u8>,
        options: CreateOptions,
    ) -> Result<crate::resource::CacheEntryResource> {
        self.inner.create_cache_entry_with(key, value, options)
    }

    /// Fetch a cache entry by key, refreshing its access time.
    #[cfg(feature = "cache")]
    pub async fn cache_get(&self, key: &str) -> Result<Option<crate::resource::CacheEntry>> {
        self.inner.cache_get(key)
    }

    // -- custom backends ------------------------------------------------------

    /// Create a resource through a user-registered backend.
    ///
    /// The backend is resolved by name at creation time; if it is not
    /// registered, creation fails with
    /// [`BackendError::Unavailable`](crate::BackendError::Unavailable).
    ///
    /// Backends may block on anything, so this offloads to the blocking
    /// pool under Tokio.
    pub async fn create_custom(
        &self,
        name: &str,
        backend: &str,
    ) -> Result<crate::resource::CustomResource> {
        self.create_custom_with(name, backend, CreateOptions::new())
            .await
    }

    /// Create a custom-backed resource with owner, metadata and TTL options.
    pub async fn create_custom_with(
        &self,
        name: &str,
        backend: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::CustomResource> {
        let inner = Arc::clone(&self.inner);
        let name = name.to_owned();
        let backend = backend.to_owned();
        offload(move || inner.create_custom_with(&name, &backend, options)).await
    }

    // -- explicit release ------------------------------------------------------

    /// Release a resource by id.
    ///
    /// Idempotent: unknown ids report `Ok(false)`; ids of other
    /// namespaces report [`RegistryError::NotFound`](crate::RegistryError::NotFound).
    /// The physical resource is destroyed via its backend before the
    /// registry entry is dropped, so this offloads to the blocking pool
    /// under Tokio.
    ///
    /// **Leases win over release.** If any lease is held on the resource,
    /// it is not destroyed: the record moves to `PendingCleanup` and this
    /// returns `Ok(false)`. Try again after the leases are gone, or let a
    /// cleanup pass finish it.
    pub async fn release(&self, id: &ResourceId) -> Result<bool> {
        let inner = Arc::clone(&self.inner);
        let id = id.clone();
        offload(move || inner.release(&id)).await
    }

    /// Re-stat a file/directory resource and update its tracked size.
    ///
    /// Returns the size that is now recorded. Memory-backed resources
    /// report the size recorded at creation. Filesystem stats go through
    /// the blocking pool under Tokio.
    pub async fn refresh_size(&self, id: &ResourceId) -> Result<u64> {
        let inner = Arc::clone(&self.inner);
        let id = id.clone();
        offload(move || inner.refresh_size(&id)).await
    }
}
