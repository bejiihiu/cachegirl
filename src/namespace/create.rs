//! Resource creation for namespaces.
//!
//! Every create path goes through the same steps: check the manager
//! is open, validate the name, take the manager-wide creation lock,
//! enforce manager and namespace quotas, materialize the resource via
//! the right backend, register it, tag it with owner/TTL/metadata and
//! emit an observability event. Creation is serialized because quota
//! checks are only atomic when they happen under this lock.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::backend::ResourceRequest;
#[cfg(any(feature = "memory", feature = "cache"))]
use crate::core::error::ConfigError;
use crate::core::error::{BackendError, ConcurrencyError, QuotaError, RegistryError, Result};
use crate::core::resource::{ResourceId, ResourceKind, ResourceMeta};
use crate::namespace::{Namespace, validate_resource_name};
use crate::observability::Event;
use crate::observability::metrics_support;

/// Free-form options applied at resource creation time.
#[derive(Clone, Debug, Default)]
pub struct CreateOptions {
    /// Owner tag, e.g. the component or request id responsible.
    pub owner: Option<String>,
    /// Fixed TTL counted from creation; falls back to the manager's `default_ttl`.
    pub ttl: Option<Duration>,
    /// Sliding TTL counted from the last access.
    pub idle_ttl: Option<Duration>,
    /// How the resource's lifetime is tied to its handles.
    ///
    /// Defaults to [`crate::Ownership::Managed`]: the resource lives
    /// until it is released or reclaimed by a cleanup pass. See
    /// [`crate::Ownership`] for the full semantics of every mode.
    pub ownership: crate::core::resource::Ownership,
    /// Eviction priority relative to other resources.
    ///
    /// When unset, the resource inherits the priority of the effective
    /// cleanup policy (namespace-specific if set, else the manager-wide
    /// one), falling back to [`crate::Priority::Normal`].
    pub priority: Option<crate::policy::Priority>,
    /// Free-form metadata attached to the registry record.
    pub metadata: HashMap<String, String>,
}

impl CreateOptions {
    /// Empty options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tag the resource with an owner.
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Give the resource a fixed TTL counted from creation,
    /// instead of the manager default.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Give the resource a sliding idle TTL counted from the last access.
    ///
    /// Cannot be combined with [`CreateOptions::ttl`]; setting both is a
    /// configuration error.
    pub fn idle_ttl(mut self, ttl: Duration) -> Self {
        self.idle_ttl = Some(ttl);
        self
    }

    /// Choose the ownership mode for the resource.
    ///
    /// See [`crate::Ownership`] for what each mode does on drop.
    pub fn ownership(mut self, ownership: crate::core::resource::Ownership) -> Self {
        self.ownership = ownership;
        self
    }

    /// Choose the eviction priority for the resource.
    ///
    /// Lower-priority resources are evicted first when a quota is
    /// exceeded; `Critical` resources are evicted last.
    pub fn priority(mut self, priority: crate::policy::Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Attach a metadata entry.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

impl Namespace {
    fn check_open(&self) -> Result<()> {
        if self.shared().closed.load(Ordering::SeqCst) {
            return Err(ConcurrencyError::ManagerClosed.into());
        }
        Ok(())
    }

    fn lock_create(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.shared()
            .create_lock
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned.into())
    }

    /// Mint the ownership core for a freshly created resource.
    fn ownership_core(
        &self,
        id: &ResourceId,
        mode: crate::core::resource::Ownership,
    ) -> std::sync::Arc<crate::ownership::OwnershipCore> {
        std::sync::Arc::new(crate::ownership::OwnershipCore::new(
            std::sync::Arc::clone(self.shared()),
            id.clone(),
            mode,
        ))
    }

    /// Bump the quota-rejection counters and log the refusal.
    fn quota_rejected(scope: &str, error: &QuotaError) {
        let reason = match error {
            QuotaError::SizeExceeded { .. } => "size",
            QuotaError::CountExceeded { .. } => "count",
            QuotaError::Namespace { .. } => "namespace",
        };
        metrics_support::quota_rejection(scope, reason);
        #[cfg(feature = "tracing")]
        tracing::warn!(scope, reason, error = %error, "quota rejected resource creation");
    }

    /// Enforce manager-wide and namespace quotas for `added_bytes`.
    ///
    /// Called with the creation lock held; the count check projects one
    /// additional resource on top of current usage.
    fn check_quotas(&self, added_bytes: u64) -> Result<()> {
        let shared = self.shared();
        let snapshot = shared.registry.snapshot()?;
        let manager_bytes: u64 = snapshot.iter().map(|m| m.size_bytes).sum();
        let manager_count = snapshot.len() as u64;

        let manager_quota = crate::namespace::NamespaceQuota {
            max_bytes: shared.config.max_size,
            max_resources: shared.config.max_resources,
        };
        if let Err(source) =
            manager_quota.check("manager", manager_bytes, manager_count, added_bytes)
        {
            Self::quota_rejected("manager", &source);
            return Err(source.into());
        }

        let (ns_bytes, ns_count) = self.usage()?;
        let ns_quota = self.quota();
        if let Err(source) = ns_quota.check(&self.name, ns_bytes, ns_count, added_bytes) {
            Self::quota_rejected(&self.name, &source);
            return Err(source.into());
        }
        Ok(())
    }

    /// Materialize via backend, register, tag, emit. Caller owns the lock.
    fn create_prepared(
        &self,
        name: &str,
        kind: ResourceKind,
        size_hint: Option<u64>,
        backend_name: Option<&str>,
        options: CreateOptions,
    ) -> Result<ResourceMeta> {
        if options.ttl.is_some() && options.idle_ttl.is_some() {
            return Err(crate::core::error::ConfigError::Invalid(
                "`ttl` and `idle_ttl` are mutually exclusive".into(),
            )
            .into());
        }
        let shared = self.shared();
        let backend = match backend_name {
            Some(name) => {
                shared
                    .backends
                    .lookup(name)
                    .ok_or_else(|| BackendError::Unavailable {
                        backend: name.to_owned(),
                        reason: "no such backend is registered".to_string(),
                    })?
            }
            None => {
                let default_name = super::Backends::name_for_kind(kind).ok_or_else(|| {
                    crate::core::error::ResourceCreationError::UnsupportedKind {
                        backend: "manager".to_string(),
                        kind: kind.to_string(),
                    }
                })?;
                shared.backends.lookup(default_name).ok_or_else(|| {
                    crate::core::error::ResourceCreationError::UnsupportedKind {
                        backend: default_name.to_string(),
                        kind: kind.to_string(),
                    }
                })?
            }
        };

        let mut request = ResourceRequest::new(name, &self.name, kind);
        if let Some(hint) = size_hint {
            request = request.with_size_hint(hint);
        }
        for (k, v) in &options.metadata {
            request.metadata.insert(k.clone(), v.clone());
        }

        let mut meta = match backend.create(&request) {
            Ok(meta) => meta,
            Err(source) => {
                metrics_support::resource_created(&self.name, kind, metrics_support::RESULT_ERROR);
                #[cfg(feature = "tracing")]
                tracing::warn!(namespace = %self.name, kind = %kind, error = %source, "backend failed to create resource");
                return Err(source);
            }
        };
        meta.ttl = options
            .idle_ttl
            .map(crate::policy::Ttl::Idle)
            .or_else(|| options.ttl.map(crate::policy::Ttl::Fixed))
            .or_else(|| shared.config.default_ttl.map(crate::policy::Ttl::Fixed));
        meta.owner = options.owner;
        meta.ownership = options.ownership;
        meta.priority = options
            .priority
            .unwrap_or_else(|| self.cleanup_policy().priority);
        for (k, v) in options.metadata {
            meta.metadata.insert(k, v);
        }
        meta.metadata
            .insert("backend".into(), backend.name().to_owned());

        if let Err(source) = shared.registry.register(meta.clone()) {
            // roll the physical side back so failed creations never leak
            let _ = backend.release(&meta);
            metrics_support::resource_created(&self.name, kind, metrics_support::RESULT_ERROR);
            #[cfg(feature = "tracing")]
            tracing::error!(namespace = %self.name, kind = %kind, id = %meta.id, error = %source, "registry failed to record the resource; creation rolled back");
            return Err(source.into());
        }
        metrics_support::resource_created(&self.name, kind, metrics_support::RESULT_OK);
        metrics_support::active_resources_change(1.0);
        #[cfg(feature = "tracing")]
        tracing::debug!(namespace = %self.name, kind = %kind, id = %meta.id, bytes = meta.size_bytes, "resource created");
        shared.emit(Event::ResourceCreated {
            instance: shared.config.instance_id.clone(),
            id: meta.id.clone(),
            kind,
            namespace: self.name.clone(),
        });
        Ok(meta)
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(
        skip(self, options),
        name = "create_resource",
        level = "debug",
        fields(namespace = %self.name, kind = %kind, name = %name),
    ))]
    fn create_with(
        &self,
        name: &str,
        kind: ResourceKind,
        size_hint: Option<u64>,
        backend_name: Option<&str>,
        options: CreateOptions,
    ) -> Result<ResourceMeta> {
        self.check_open()?;
        validate_resource_name(name)?;
        let _guard = self.lock_create()?;
        self.check_open()?; // shutdown may land between the check and the lock
        self.check_quotas(size_hint.unwrap_or(0))?;
        self.create_prepared(name, kind, size_hint, backend_name, options)
    }

    // -- filesystem resources ---------------------------------------------

    /// Create an empty file `root/<instance>/<namespace>/<name>`.
    #[cfg(feature = "filesystem")]
    pub fn create_file(&self, name: &str) -> Result<crate::resource::FileResource> {
        self.create_file_with(name, CreateOptions::new())
    }

    /// Create an empty file with owner, metadata and TTL options.
    #[cfg(feature = "filesystem")]
    pub fn create_file_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::FileResource> {
        let meta = self.create_with(name, ResourceKind::File, None, None, options)?;
        let path = std::path::PathBuf::from(meta.metadata.get("path").cloned().unwrap_or_default());
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::FileResource::new(meta.id, path, core))
    }

    /// Create a directory `root/<instance>/<namespace>/<name>`.
    #[cfg(feature = "filesystem")]
    pub fn create_directory(&self, name: &str) -> Result<crate::resource::DirectoryResource> {
        self.create_directory_with(name, CreateOptions::new())
    }

    /// Create a directory with owner, metadata and TTL options.
    #[cfg(feature = "filesystem")]
    pub fn create_directory_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::DirectoryResource> {
        let meta = self.create_with(name, ResourceKind::Directory, None, None, options)?;
        let path = std::path::PathBuf::from(meta.metadata.get("path").cloned().unwrap_or_default());
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::DirectoryResource::new(meta.id, path, core))
    }

    // -- in-memory resources ----------------------------------------------

    /// Allocate a buffer of `capacity` bytes.
    ///
    /// The bytes are accounted against manager and namespace quotas and
    /// are released from the registry by [`Namespace::release`].
    #[cfg(feature = "memory")]
    pub fn create_buffer(&self, capacity: usize) -> Result<crate::resource::BufferResource> {
        self.create_buffer_with(capacity, CreateOptions::new())
    }

    /// Allocate a buffer with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub fn create_buffer_with(
        &self,
        capacity: usize,
        options: CreateOptions,
    ) -> Result<crate::resource::BufferResource> {
        let hint = u64::try_from(capacity)
            .map_err(|_| ConfigError::Invalid("buffer capacity does not fit into u64".into()))?;
        let name = anonymous_name("buffer");
        let meta = self.create_with(&name, ResourceKind::Buffer, Some(hint), None, options)?;
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::BufferResource::new(
            meta.id,
            crate::resource::Buffer::with_capacity(capacity),
            core,
        ))
    }

    /// Store a serialization intermediate registered under `name`.
    #[cfg(feature = "memory")]
    pub fn create_serialization(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<crate::resource::SerializationResource> {
        self.create_serialization_with(name, bytes, CreateOptions::new())
    }

    /// Store a serialization intermediate with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub fn create_serialization_with(
        &self,
        name: &str,
        bytes: Vec<u8>,
        options: CreateOptions,
    ) -> Result<crate::resource::SerializationResource> {
        let size = u64::try_from(bytes.len())
            .map_err(|_| ConfigError::Invalid("payload does not fit into u64".into()))?;
        let meta =
            self.create_with(name, ResourceKind::Serialization, Some(size), None, options)?;
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::SerializationResource::new(
            meta.id, bytes, core,
        ))
    }

    /// Register a runtime-scoped resource tied to the current operation or session.
    ///
    /// Carries no bytes; it exists so lifetime-scoped objects can be
    /// tracked, quota-counted and cleaned up like everything else.
    #[cfg(feature = "memory")]
    pub fn create_runtime(&self, name: &str) -> Result<crate::resource::RuntimeResource> {
        self.create_runtime_with(name, CreateOptions::new())
    }

    /// Register a runtime-scoped resource with owner, metadata and TTL options.
    #[cfg(feature = "memory")]
    pub fn create_runtime_with(
        &self,
        name: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::RuntimeResource> {
        let meta = self.create_with(name, ResourceKind::Runtime, Some(0), None, options)?;
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::RuntimeResource::new(
            meta.id, meta.name, core,
        ))
    }

    // -- cache resources ----------------------------------------------------

    /// Store a cache entry under `key`.
    ///
    /// Keys are unique within a namespace; creating the same key twice
    /// is rejected. Entries are retrievable with [`Namespace::cache_get`].
    #[cfg(feature = "cache")]
    pub fn create_cache_entry(
        &self,
        key: &str,
        value: Vec<u8>,
    ) -> Result<crate::resource::CacheEntryResource> {
        self.create_cache_entry_with(key, value, CreateOptions::new())
    }

    /// Store a cache entry with owner, metadata and TTL options.
    #[cfg(feature = "cache")]
    pub fn create_cache_entry_with(
        &self,
        key: &str,
        value: Vec<u8>,
        options: CreateOptions,
    ) -> Result<crate::resource::CacheEntryResource> {
        self.check_open()?;
        validate_resource_name(key)?;
        let _guard = self.lock_create()?;
        self.check_open()?;
        let size = u64::try_from(value.len())
            .map_err(|_| ConfigError::Invalid("payload does not fit into u64".into()))?;
        self.check_quotas(size)?;
        let shared = self.shared();
        if shared.backends.cache_store.contains(&self.name, key) {
            return Err(BackendError::Rejected {
                backend: "cache".to_string(),
                reason: format!(
                    "cache key `{key}` already exists in namespace `{}`",
                    self.name
                ),
            }
            .into());
        }
        let mut options = options;
        options.metadata.insert("cache_key".into(), key.to_owned());
        let meta = self.create_prepared(key, ResourceKind::Cache, Some(size), None, options)?;
        let entry = std::sync::Arc::new(std::sync::Mutex::new(crate::resource::CacheEntry::new(
            key, value,
        )));
        shared.backends.cache_store.insert(
            &self.name,
            key,
            meta.id.clone(),
            std::sync::Arc::clone(&entry),
        );
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::CacheEntryResource::new(
            meta.id,
            key.to_owned(),
            entry,
            core,
        ))
    }

    /// Fetch a cache entry by key, refreshing its access time.
    #[cfg(feature = "cache")]
    pub fn cache_get(&self, key: &str) -> Result<Option<crate::resource::CacheEntry>> {
        self.check_open()?;
        validate_resource_name(key)?;
        let shared = self.shared();
        let Some(stored) = shared.backends.cache_store.get(&self.name, key) else {
            return Ok(None);
        };
        let snapshot = {
            let mut guard = stored
                .entry
                .lock()
                .map_err(|_| ConcurrencyError::Poisoned)?;
            guard.touch();
            guard.clone()
        };
        // best-effort registry bookkeeping; a concurrent release may win the race
        let _ = shared.registry.update(
            &stored.id,
            crate::core::resource::ResourceMeta::record_access,
        );
        Ok(Some(snapshot))
    }

    // -- custom backends ----------------------------------------------------

    /// Create a resource through a user-registered backend.
    ///
    /// The backend is resolved by name at creation time; if it is not
    /// registered, creation fails with [`BackendError::Unavailable`].
    pub fn create_custom(
        &self,
        name: &str,
        backend: &str,
    ) -> Result<crate::resource::CustomResource> {
        self.create_custom_with(name, backend, CreateOptions::new())
    }

    /// Create a custom-backed resource with owner, metadata and TTL options.
    pub fn create_custom_with(
        &self,
        name: &str,
        backend: &str,
        options: CreateOptions,
    ) -> Result<crate::resource::CustomResource> {
        let meta = self.create_with(name, ResourceKind::Custom, None, Some(backend), options)?;
        let core = self.ownership_core(&meta.id, meta.ownership);
        Ok(crate::resource::CustomResource::new(
            meta.id,
            backend.to_owned(),
            core,
        ))
    }

    // -- explicit release ---------------------------------------------------

    /// Release a resource by id.
    ///
    /// Idempotent: unknown ids (or ids of other namespaces) report
    /// `Ok(false)` / [`RegistryError::NotFound`] respectively. The
    /// physical resource is destroyed via its backend before the registry
    /// entry is dropped, and a release event is emitted.
    ///
    /// **Leases win over release.** If any lease is held on the resource,
    /// it is not destroyed: the record moves to `PendingCleanup`, the
    /// entry stays in the registry, and this returns `Ok(false)`. Try
    /// again after the leases are gone, or let a cleanup pass finish it.
    pub fn release(&self, id: &crate::core::resource::ResourceId) -> Result<bool> {
        let shared = self.shared();
        let meta = match shared.registry.get(id) {
            Ok(meta) => meta,
            Err(RegistryError::NotFound(_)) => return Ok(false),
            Err(source) => return Err(source.into()),
        };
        if meta.namespace != self.name {
            // hide other namespaces' resources entirely
            return Err(RegistryError::NotFound(id.clone()).into());
        }

        match crate::ownership::destroy_resource(shared, id, "release")? {
            crate::ownership::DestroyOutcome::Destroyed => Ok(true),
            crate::ownership::DestroyOutcome::AlreadyGone
            | crate::ownership::DestroyOutcome::Deferred => Ok(false),
        }
    }

    /// Re-stat a file/directory resource and update its tracked size.
    ///
    /// Returns the size that is now recorded. Memory-backed resources
    /// report the size recorded at creation.
    pub fn refresh_size(&self, id: &crate::core::resource::ResourceId) -> Result<u64> {
        let shared = self.shared();
        let meta = match shared.registry.get(id) {
            Ok(meta) => meta,
            Err(RegistryError::NotFound(_)) => return Ok(0),
            Err(source) => return Err(source.into()),
        };
        if meta.namespace != self.name {
            return Err(RegistryError::NotFound(id.clone()).into());
        }
        let size = match meta.kind {
            #[cfg(feature = "filesystem")]
            ResourceKind::File | ResourceKind::Directory => match meta.metadata.get("path") {
                Some(path) => std::fs::metadata(path)
                    .map_err(|source| BackendError::Io {
                        backend: "filesystem".to_string(),
                        source,
                    })?
                    .len(),
                None => meta.size_bytes,
            },
            _ => meta.size_bytes,
        };
        if size != meta.size_bytes {
            shared.registry.update(id, |m| m.size_bytes = size)?;
        }
        Ok(size)
    }
}

#[cfg(feature = "memory")]
fn anonymous_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering as AOrd};
    use std::time::SystemTime;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tick = COUNTER.fetch_add(1, AOrd::Relaxed);
    format!("{prefix}-{nanos:x}-{tick:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::Error;
    use crate::core::lifecycle::ResourceState;
    use crate::core::manager::TempManager;
    use crate::core::resource::ResourceKind;
    use crate::observability::{Event, VecSink};
    use std::sync::Arc;
    use std::time::Duration;

    fn manager(dir: &std::path::Path) -> TempManager {
        TempManager::builder()
            .root(dir)
            .instance_id("instance-create")
            .build()
            .unwrap()
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn file_appears_on_disk_and_is_registered() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("protocol").unwrap();

        let file = ns.create_file("packet-dump.bin").unwrap();
        assert!(file.path().is_file());
        // backends canonicalize, so compare against the canonical root
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(file.path().starts_with(&root));
        assert!(
            file.path()
                .starts_with(root.join("instance-create").join("protocol"))
        );

        let meta = manager.registry().get(file.id()).unwrap();
        assert_eq!(meta.kind, ResourceKind::File);
        assert_eq!(meta.namespace, "protocol");
        assert_eq!(meta.name, "packet-dump.bin");
        assert_eq!(meta.state, ResourceState::Created);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn directory_is_created_on_disk() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("scratch").unwrap();

        let entries = ns.create_directory("workdir").unwrap();
        assert!(entries.path().is_dir());
        assert!(entries.exists());
        let meta = manager.registry().get(entries.id()).unwrap();
        assert_eq!(meta.kind, ResourceKind::Directory);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn duplicate_file_name_is_rejected() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("protocol").unwrap();
        ns.create_file("dump.bin").unwrap();
        assert!(ns.create_file("dump.bin").is_err());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn buffer_is_allocated_and_accounted() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("memory").unwrap();

        let mut buf = ns.create_buffer(64 * 1024).unwrap();
        assert_eq!(buf.capacity(), 64 * 1024);
        buf.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(buf.as_slice(), &[1, 2, 3, 4]);

        let meta = manager.registry().get(buf.id()).unwrap();
        assert_eq!(meta.kind, ResourceKind::Buffer);
        assert_eq!(meta.size_bytes, (64 * 1024) as u64);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn namespace_size_quota_blocks_creation() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("memory").unwrap();
        ns.set_quota(crate::namespace::NamespaceQuota::with_max_bytes(100));

        let _first = ns.create_buffer(64).unwrap();
        let err = ns.create_buffer(64).unwrap_err();
        assert!(matches!(
            err,
            Error::Quota(crate::core::error::QuotaError::SizeExceeded { .. })
        ));
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn namespace_count_quota_blocks_creation() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("counting").unwrap();
        ns.set_quota(crate::namespace::NamespaceQuota::with_max_resources(1));

        let _first = ns.create_buffer(8).unwrap();
        let err = ns.create_buffer(8).unwrap_err();
        assert!(matches!(
            err,
            Error::Quota(crate::core::error::QuotaError::CountExceeded { .. })
        ));
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn manager_wide_count_quota_blocks_creation() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_resources(1)
            .build()
            .unwrap();
        let ns = manager.namespace("limited").unwrap();

        ns.create_file("one.bin").unwrap();
        let err = ns.create_file("two.bin").unwrap_err();
        assert!(matches!(
            err,
            Error::Quota(crate::core::error::QuotaError::CountExceeded { .. })
        ));
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn manager_wide_size_quota_blocks_creation() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_size(128)
            .build()
            .unwrap();
        let ns = manager.namespace("limited").unwrap();

        let err = ns.create_buffer(256).unwrap_err();
        assert!(matches!(
            err,
            Error::Quota(crate::core::error::QuotaError::SizeExceeded { .. })
        ));
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn quota_failure_leaves_nothing_behind() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_size(100)
            .build()
            .unwrap();
        let ns = manager.namespace("limited").unwrap();

        assert!(ns.create_buffer(256).is_err());
        assert_eq!(manager.registry().len().unwrap(), 0);
        let stats = manager.stats().unwrap();
        assert_eq!(stats.resources, 0);
        assert_eq!(stats.bytes_tracked, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn default_ttl_is_applied_and_overridable() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .default_ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        let ns = manager.namespace("ttl").unwrap();

        let file = ns.create_file("with-default.bin").unwrap();
        let meta = manager.registry().get(file.id()).unwrap();
        assert_eq!(
            meta.ttl,
            Some(crate::policy::Ttl::Fixed(Duration::from_secs(3600)))
        );

        let custom = ns
            .create_file_with(
                "with-override.bin",
                CreateOptions::new().ttl(Duration::from_secs(5)),
            )
            .unwrap();
        let meta = manager.registry().get(custom.id()).unwrap();
        assert_eq!(
            meta.ttl,
            Some(crate::policy::Ttl::Fixed(Duration::from_secs(5)))
        );
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn owner_and_metadata_are_recorded() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("owned").unwrap();

        let file = ns
            .create_file_with(
                "packet.bin",
                CreateOptions::new()
                    .owner("network")
                    .metadata("request", "req-42"),
            )
            .unwrap();
        let meta = manager.registry().get(file.id()).unwrap();
        assert_eq!(meta.owner.as_deref(), Some("network"));
        assert_eq!(
            meta.metadata.get("request").map(String::as_str),
            Some("req-42")
        );
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn lifecycle_events_are_emitted() {
        let dir = crate::testsupport::TestDir::new();
        let sink = Arc::new(VecSink::new());
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .event_sink(sink.clone())
            .build()
            .unwrap();
        let ns = manager.namespace("observed").unwrap();

        let file = ns.create_file("probe.bin").unwrap();
        let id = file.id().clone();
        ns.release(&id).unwrap();

        let events = sink.drain();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            Event::ResourceCreated { kind, namespace, id: event_id, .. }
                if *kind == ResourceKind::File && namespace == "observed" && *event_id == id
        ));
        assert!(matches!(
            &events[1],
            Event::ResourceReleased { id: event_id, .. } if *event_id == id
        ));
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn release_updates_statistics() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("stats").unwrap();

        let file = ns.create_file("count-me.bin").unwrap();
        let id = file.id().clone();
        ns.release(&id).unwrap();

        let stats = manager.stats().unwrap();
        assert_eq!(stats.resources, 0);
        assert_eq!(stats.resources_released, 1);
        assert_eq!(stats.bytes_tracked, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn release_is_idempotent_and_namespace_scoped() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("mine").unwrap();
        let other = manager.namespace("theirs").unwrap();

        let file = ns.create_file("private.bin").unwrap();
        let id = file.id().clone();

        // another namespace may not release it
        let err = other.release(&id).unwrap_err();
        assert!(matches!(err, Error::Registry(RegistryError::NotFound(_))));
        assert!(ns.release(&id).unwrap());
        // second release is a no-op, not an error
        assert!(!ns.release(&id).unwrap());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn refresh_size_tracks_file_growth() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("sized").unwrap();

        let file = ns.create_file("growing.bin").unwrap();
        std::fs::write(file.path(), vec![0u8; 42]).unwrap();
        let size = ns.refresh_size(file.id()).unwrap();
        assert_eq!(size, 42);
        let meta = manager.registry().get(file.id()).unwrap();
        assert_eq!(meta.size_bytes, 42);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "cache")]
    #[test]
    fn cache_entry_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("cache").unwrap();

        let entry = ns.create_cache_entry("pkt-1", vec![9, 8, 7]).unwrap();
        assert_eq!(entry.len(), 3);
        assert_eq!(entry.value().unwrap(), vec![9, 8, 7]);

        let fetched = ns
            .cache_get("pkt-1")
            .unwrap()
            .expect("entry must be cached");
        assert_eq!(fetched.value, vec![9, 8, 7]);
        // registry bookkeeping is refreshed too
        let meta = manager.registry().get(entry.id()).unwrap();
        assert!(meta.last_accessed >= fetched.created_at);

        // duplicate keys are rejected
        assert!(ns.create_cache_entry("pkt-1", vec![1]).is_err());

        let id = entry.id().clone();
        ns.release(&id).unwrap();
        assert!(ns.cache_get("pkt-1").unwrap().is_none());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "cache")]
    #[test]
    fn cache_entry_accounts_bytes() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("cache").unwrap();

        let entry = ns.create_cache_entry("blob", vec![0u8; 512]).unwrap();
        let meta = manager.registry().get(entry.id()).unwrap();
        assert_eq!(meta.size_bytes, 512);
        assert_eq!(meta.kind, ResourceKind::Cache);
        let stats = manager.stats().unwrap();
        assert_eq!(stats.bytes_tracked, 512);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn serialization_resource_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("ser").unwrap();

        let res = ns
            .create_serialization("frame-1", vec![1, 3, 3, 7])
            .unwrap();
        assert_eq!(res.as_slice(), &[1, 3, 3, 7]);
        assert_eq!(res.len(), 4);
        let meta = manager.registry().get(res.id()).unwrap();
        assert_eq!(meta.kind, ResourceKind::Serialization);
        assert_eq!(meta.size_bytes, 4);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn runtime_resource_is_tracked() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("runtime").unwrap();

        let res = ns
            .create_runtime_with("req-42", CreateOptions::new().owner("billing"))
            .unwrap();
        assert_eq!(res.name(), "req-42");
        let meta = manager.registry().get(res.id()).unwrap();
        assert_eq!(meta.kind, ResourceKind::Runtime);
        assert_eq!(meta.owner.as_deref(), Some("billing"));
        manager.shutdown().unwrap();
    }

    struct CountingBackend {
        created: std::sync::atomic::AtomicU64,
        released: std::sync::atomic::AtomicU64,
    }

    impl crate::backend::ResourceBackend for CountingBackend {
        fn name(&self) -> &str {
            "counting"
        }

        fn create(&self, request: &ResourceRequest) -> Result<ResourceMeta> {
            self.created.fetch_add(1, Ordering::Relaxed);
            Ok(ResourceMeta::new(
                &request.name,
                &request.namespace,
                request.kind,
            ))
        }

        fn release(&self, _meta: &ResourceMeta) -> Result<()> {
            self.released.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn custom_backend_create_and_release() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let backend = Arc::new(CountingBackend {
            created: std::sync::atomic::AtomicU64::new(0),
            released: std::sync::atomic::AtomicU64::new(0),
        });
        manager.register_backend(backend.clone()).unwrap();

        let ns = manager.namespace("custom").unwrap();
        let res = ns.create_custom("widget", "counting").unwrap();
        assert_eq!(res.backend(), "counting");
        assert_eq!(backend.created.load(Ordering::Relaxed), 1);

        let id = res.id().clone();
        assert!(ns.release(&id).unwrap());
        assert_eq!(backend.released.load(Ordering::Relaxed), 1);
        assert_eq!(manager.registry().len().unwrap(), 0);
        manager.shutdown().unwrap();
    }

    #[test]
    fn unknown_custom_backend_fails() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("custom").unwrap();

        let err = ns.create_custom("widget", "missing").unwrap_err();
        assert!(matches!(
            err,
            Error::Backend(BackendError::Unavailable { .. })
        ));
        assert_eq!(manager.registry().len().unwrap(), 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn concurrent_creation_respects_manager_quota() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_resources(30)
            .build()
            .unwrap();
        let ns = manager.namespace("parallel").unwrap();
        let ns = Arc::new(ns);

        let mut handles = Vec::new();
        for worker in 0..6 {
            let ns = Arc::clone(&ns);
            handles.push(std::thread::spawn(move || {
                let mut created = 0u32;
                let mut rejected = 0u32;
                for n in 0..10 {
                    match ns.create_file(&format!("worker-{worker}-{n}.bin")) {
                        Ok(_) => created += 1,
                        Err(Error::Quota(_)) => rejected += 1,
                        Err(other) => panic!("unexpected error: {other}"),
                    }
                }
                (created, rejected)
            }));
        }
        let (created, rejected) = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .fold((0u32, 0u32), |acc, x| (acc.0 + x.0, acc.1 + x.1));

        assert_eq!(manager.registry().len().unwrap(), 30);
        assert_eq!(created, 30);
        assert_eq!(rejected, 30);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn concurrent_buffer_creation_is_accounted() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_size(1024)
            .build()
            .unwrap();
        let ns = Arc::new(manager.namespace("parallel").unwrap());

        let mut handles = Vec::new();
        for _ in 0..4 {
            let ns = Arc::clone(&ns);
            handles.push(std::thread::spawn(move || {
                let mut ok = 0u32;
                for _ in 0..8 {
                    if ns.create_buffer(128).is_ok() {
                        ok += 1;
                    }
                }
                ok
            }));
        }
        let total_ok: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total_ok, 8);
        let stats = manager.stats().unwrap();
        assert_eq!(stats.resources, 8);
        assert_eq!(stats.bytes_tracked, 1024);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn creation_fails_after_shutdown() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("closing").unwrap();
        manager.shutdown().unwrap();

        let err = ns.create_file("too-late.bin").unwrap_err();
        assert!(matches!(
            err,
            Error::Concurrency(ConcurrencyError::ManagerClosed)
        ));
    }

    #[cfg(feature = "filesystem")]
    #[cfg(feature = "filesystem")]
    #[test]
    fn owned_drop_deletes_file_from_disk() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("owned-drop").unwrap();

        let file = ns
            .create_file_with(
                "owned.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Owned),
            )
            .unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();
        assert!(path.is_file());
        drop(file);

        // physical side is gone, registry is empty, stats moved
        assert!(!path.exists());
        assert!(manager.registry().get(&id).is_err());
        let stats = manager.stats().unwrap();
        assert_eq!(stats.resources_released, 1);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn managed_drop_keeps_file_and_registration() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("managed-drop").unwrap();

        let file = ns.create_file("managed.bin").unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();
        drop(file);

        assert!(path.is_file());
        assert!(manager.registry().get(&id).is_ok());
        assert!(ns.release(&id).unwrap());
        assert!(!path.exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn shared_last_drop_destroys_the_file() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("shared-drop").unwrap();

        let file = ns
            .create_file_with(
                "shared.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Shared),
            )
            .unwrap();
        let path = file.path().to_path_buf();
        let clone = file.clone();
        drop(file);
        // one owner is still around: nothing may be destroyed
        assert!(path.is_file());
        drop(clone);

        assert!(!path.exists());
        assert_eq!(manager.registry().len().unwrap(), 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn leased_release_defers_until_leases_drop() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("leased").unwrap();

        let file = ns
            .create_file_with(
                "leased.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Leased),
            )
            .unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();
        let lease = file.acquire().unwrap();

        // release must refuse to destroy while the lease is held
        assert!(!ns.release(&id).unwrap());
        assert!(path.is_file());
        let meta = manager.registry().get(&id).unwrap();
        assert_eq!(meta.state, ResourceState::PendingCleanup);

        drop(lease);
        assert!(ns.release(&id).unwrap());
        assert!(!path.exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn cleanup_never_destroys_a_leased_resource() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("cleanup-lease").unwrap();

        let file = ns.create_file("held.bin").unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();
        let lease = file.acquire().unwrap();

        manager
            .registry()
            .set_state(&id, ResourceState::Expired)
            .unwrap();

        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.deferred, 1);
        assert!(path.is_file());
        // the lease is untouched by the failed destruction attempt
        assert_eq!(manager.stats().unwrap().active_leases, 1);

        drop(lease);
        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.removed, 1);
        assert!(!path.exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn owned_drop_under_lease_defers_then_cleanup_finishes() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("owned-leased").unwrap();

        let file = ns
            .create_file_with(
                "owned-held.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Owned),
            )
            .unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();
        let lease = file.acquire().unwrap();
        drop(file); // owner is gone, lease still held

        assert!(path.is_file());
        let meta = manager.registry().get(&id).unwrap();
        assert_eq!(meta.state, ResourceState::PendingCleanup);

        drop(lease);
        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.removed, 1);
        assert!(!path.exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn concurrent_leases_on_one_resource_are_all_counted() {
        use std::sync::Barrier;

        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("concurrent-leases").unwrap();
        let file = Arc::new(ns.create_file("hot.bin").unwrap());

        let barrier = Arc::new(Barrier::new(5));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let file = Arc::clone(&file);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let mut leases = Vec::new();
                for _ in 0..25 {
                    leases.push(file.acquire().unwrap());
                }
                (leases.len(), leases)
            }));
        }
        barrier.wait();
        let keepers: Vec<_> = handles.into_iter().map(|h| h.join().unwrap().1).collect();
        let total: usize = keepers.iter().map(Vec::len).sum();
        assert_eq!(total, 100);
        assert_eq!(manager.stats().unwrap().active_leases, 100);

        drop(keepers);
        assert_eq!(manager.stats().unwrap().active_leases, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn lease_can_move_to_another_thread() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("lease-move").unwrap();
        let file = ns.create_file("portable.bin").unwrap();
        let lease = file.acquire().unwrap();
        assert_eq!(manager.stats().unwrap().active_leases, 1);

        std::thread::spawn(move || {
            drop(lease);
        })
        .join()
        .unwrap();
        assert_eq!(manager.stats().unwrap().active_leases, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn panicking_leaseholder_does_not_leak_counts() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("panic-lease").unwrap();
        let file = ns.create_file("unwind.bin").unwrap();

        let before = manager.stats().unwrap().active_leases;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = file.acquire().unwrap();
            panic!("simulated worker failure");
        }));
        assert!(result.is_err());
        assert_eq!(manager.stats().unwrap().active_leases, before);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn deferred_release_still_counts_against_quota() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-create")
            .max_size(1024)
            .build()
            .unwrap();
        let ns = manager.namespace("quota-lease").unwrap();

        let buf = ns.create_buffer(512).unwrap();
        let id = buf.id().clone();
        let lease = buf.acquire().unwrap();

        // release is deferred, bytes must stay accounted
        assert!(!ns.release(&id).unwrap());
        assert!(ns.create_buffer(600).is_err());

        drop(lease);
        // the deferred record is pending cleanup and gets reclaimed here
        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.removed, 1);

        // bytes are free again, creation works
        let _second = ns.create_buffer(600).unwrap();
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn statistics_track_active_leases_across_resources() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("lease-stats").unwrap();

        let a = ns.create_file("a.bin").unwrap();
        let b = ns.create_file("b.bin").unwrap();
        let l1 = a.acquire().unwrap();
        let l2 = a.acquire().unwrap();
        let l3 = b.acquire().unwrap();
        assert_eq!(manager.stats().unwrap().active_leases, 3);

        drop(l1);
        assert_eq!(manager.stats().unwrap().active_leases, 2);
        drop(l2);
        drop(l3);
        assert_eq!(manager.stats().unwrap().active_leases, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "memory")]
    #[test]
    fn owned_buffer_drop_releases_backend_accounting() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("owned-buffer").unwrap();

        let buf = ns
            .create_buffer_with(
                128,
                CreateOptions::new().ownership(crate::core::resource::Ownership::Owned),
            )
            .unwrap();
        let id = buf.id().clone();
        assert_eq!(manager.registry().get(&id).unwrap().size_bytes, 128);
        drop(buf);

        assert!(manager.registry().get(&id).is_err());
        assert_eq!(manager.stats().unwrap().bytes_tracked, 0);
        assert_eq!(manager.stats().unwrap().resources, 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "cache")]
    #[test]
    fn owned_cache_entry_drop_removes_it_from_the_store() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("owned-cache").unwrap();

        let entry = ns
            .create_cache_entry_with(
                "pkt-owned",
                vec![1, 2, 3],
                CreateOptions::new().ownership(crate::core::resource::Ownership::Owned),
            )
            .unwrap();
        assert!(ns.cache_get("pkt-owned").unwrap().is_some());
        drop(entry);

        assert!(ns.cache_get("pkt-owned").unwrap().is_none());
        assert_eq!(manager.stats().unwrap().bytes_tracked, 0);
        manager.shutdown().unwrap();
    }

    #[test]
    fn owned_custom_resource_drop_releases_exactly_once_across_threads() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let backend = Arc::new(CountingBackend {
            created: std::sync::atomic::AtomicU64::new(0),
            released: std::sync::atomic::AtomicU64::new(0),
        });
        manager.register_backend(backend.clone()).unwrap();

        let ns = manager.namespace("owned-custom").unwrap();
        let res = ns
            .create_custom_with(
                "widget",
                "counting",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Shared),
            )
            .unwrap();
        let clone = res.clone();

        // both clones drop concurrently on different threads; the
        // backend must observe exactly one release
        let t = std::thread::spawn(move || drop(clone));
        drop(res);
        t.join().unwrap();
        assert_eq!(backend.released.load(Ordering::Relaxed), 1);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn drop_and_concurrent_release_destroy_exactly_once() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("drop-vs-release").unwrap();

        let file = ns
            .create_file_with(
                "race.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Owned),
            )
            .unwrap();
        let path = file.path().to_path_buf();
        let id = file.id().clone();

        let ns2 = manager.namespace("drop-vs-release").unwrap();
        let id2 = id.clone();
        let releaser = std::thread::spawn(move || ns2.release(&id2));
        drop(file); // owned drop races the explicit release
        let _ = releaser.join().unwrap().unwrap();

        // whoever won, the file is gone and the registry is empty
        assert!(!path.exists());
        assert!(manager.registry().get(&id).is_err());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn shared_clones_cross_threads() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("shared-threads").unwrap();

        let file = ns
            .create_file_with(
                "shared-thread.bin",
                CreateOptions::new().ownership(crate::core::resource::Ownership::Shared),
            )
            .unwrap();
        let path = file.path().to_path_buf();

        let mut handles = Vec::new();
        for _ in 0..3 {
            let clone = file.clone();
            handles.push(std::thread::spawn(move || drop(clone)));
        }
        drop(file);
        for h in handles {
            h.join().unwrap();
        }

        assert!(!path.exists());
        assert_eq!(manager.registry().len().unwrap(), 0);
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn invalid_resource_names_are_rejected() {
        let dir = crate::testsupport::TestDir::new();
        let manager = manager(dir.path());
        let ns = manager.namespace("names").unwrap();

        assert!(ns.create_file("../escape.bin").is_err());
        assert!(ns.create_file("a/b.bin").is_err());
        assert!(ns.create_file("").is_err());
        assert_eq!(manager.registry().len().unwrap(), 0);
        manager.shutdown().unwrap();
    }
}
