//! Plumbing shared between the manager and its namespaces.
//!
//! The manager builds one [`NamespaceShared`] at startup and hands a
//! clone to every namespace it creates. That is how namespaces reach
//! the backends, the manager-wide configuration, the event sink and
//! the creation lock without globals or I/O in the registry.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock, Weak};

use crate::backend::{BackendRegistry, ResourceBackend};
use crate::core::manager::ManagerConfig;
use crate::core::registry::Registry;
use crate::core::resource::ResourceKind;
use crate::namespace::Namespace;
use crate::observability::{Event, EventSink, StatisticsCollector};

/// Built-in backends plus the user's custom table.
pub(crate) struct Backends {
    builtins: HashMap<&'static str, Arc<dyn ResourceBackend>>,
    /// User-registered backends; filled through `TempManager::register_backend`.
    pub(crate) custom: Arc<BackendRegistry>,
    #[cfg(feature = "cache")]
    pub(crate) cache_store: Arc<crate::resource::cache::CacheStore>,
}

impl Backends {
    pub(crate) fn new(
        #[cfg(feature = "filesystem")] root: &std::path::Path,
        #[cfg(feature = "filesystem")] instance_id: &str,
        #[cfg(feature = "filesystem")] symlink_policy: crate::core::manager::SymlinkPolicy,
    ) -> crate::core::error::Result<Self> {
        #[allow(unused_mut)]
        let mut builtins: HashMap<&'static str, Arc<dyn ResourceBackend>> = HashMap::new();
        #[cfg(feature = "filesystem")]
        {
            let fs = crate::backend::FilesystemBackend::new(root, instance_id, symlink_policy)?;
            builtins.insert("filesystem", Arc::new(fs));
        }
        #[cfg(feature = "memory")]
        {
            builtins.insert("memory", Arc::new(crate::backend::MemoryBackend::new()));
        }
        #[cfg(not(any(feature = "filesystem", feature = "memory")))]
        let _ = &builtins;
        Ok(Self {
            builtins,
            custom: Arc::new(BackendRegistry::new()),
            #[cfg(feature = "cache")]
            cache_store: Arc::new(crate::resource::cache::CacheStore::new()),
        })
    }

    /// Look a backend up by name: built-ins first, then custom ones.
    pub(crate) fn lookup(&self, name: &str) -> Option<Arc<dyn ResourceBackend>> {
        self.builtins
            .get(name)
            .cloned()
            .or_else(|| self.custom.get(name))
    }

    /// Default backend name for a resource kind, if one exists.
    pub(crate) fn name_for_kind(kind: ResourceKind) -> Option<&'static str> {
        match kind {
            ResourceKind::File | ResourceKind::Directory => {
                cfg!(feature = "filesystem").then_some("filesystem")
            }
            ResourceKind::Buffer
            | ResourceKind::Cache
            | ResourceKind::Serialization
            | ResourceKind::Runtime => cfg!(feature = "memory").then_some("memory"),
            ResourceKind::Custom => None,
        }
    }
}

/// Everything a namespace needs from its manager.
pub(crate) struct NamespaceShared {
    /// Manager configuration (quotas, default TTL, instance id).
    pub(crate) config: Arc<ManagerConfig>,
    /// The manager-wide registry.
    pub(crate) registry: Arc<Registry>,
    /// Backends available to namespaces of this manager.
    pub(crate) backends: Backends,
    /// Counters feeding `Statistics`.
    pub(crate) counters: Arc<StatisticsCollector>,
    /// The manager-wide lease book; resources hand leases out from here.
    pub(crate) leases: Arc<crate::ownership::LeaseBook>,
    /// Where lifecycle events go.
    pub(crate) sink: Arc<dyn EventSink>,
    /// Weak view of the manager's namespace registry.
    ///
    /// Used to resolve ancestor quotas at creation time and descendant
    /// sweep views at cleanup time. `Weak`, not `Arc`: the manager
    /// owns the map and the namespaces inside it point back at this
    /// plumbing — an owning reference would cycle and leak everything.
    pub(crate) namespaces: Weak<Mutex<HashMap<String, Arc<Namespace>>>>,
    /// Shared shutdown flag; set by `TempManager::shutdown`.
    pub(crate) closed: Arc<AtomicBool>,
    /// Manager-wide cleanup policy, when one has been configured.
    pub(crate) cleanup_policy: RwLock<Option<crate::policy::CleanupPolicy>>,
    /// Manager-wide custom cleanup policy, when one has been configured.
    pub(crate) custom_cleanup_policy: RwLock<Option<Arc<dyn crate::policy::CustomCleanupPolicy>>>,
}

impl NamespaceShared {
    pub(crate) fn emit(&self, event: Event) {
        self.sink.emit(&event);
    }

    /// Get or create a namespace, registering every level of the path.
    ///
    /// `full` must be a pre-validated namespace path (a segment or a
    /// `/`-joined chain of segments). Every prefix of the path is
    /// materialized — the directory on disk (with the `filesystem`
    /// feature), the metadata object and the registry entry in the
    /// manager's namespace map — because later operations (quota
    /// reservations, recursive cleanup, statistics) walk that map and
    /// need every level to be there.
    pub(crate) fn get_or_create_namespace(
        self: &Arc<Self>,
        full: &str,
    ) -> crate::core::error::Result<Arc<Namespace>> {
        use crate::core::error::ConcurrencyError;

        // callers validate already; re-check the cheap invariants so a
        // malformed path can never reach the filesystem
        crate::namespace::validate_namespace_path(full)?;
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ConcurrencyError::ManagerClosed.into());
        }
        let map = self
            .namespaces
            .upgrade()
            .ok_or(ConcurrencyError::ManagerClosed)?;
        let mut guard = map.lock().map_err(|_| ConcurrencyError::Poisoned)?;
        if let Some(existing) = guard.get(full) {
            return Ok(Arc::clone(existing));
        }
        #[cfg(feature = "filesystem")]
        {
            let mut dir = self.config.root.join(&self.config.instance_id);
            for segment in full.split('/') {
                dir.push(segment);
            }
            std::fs::create_dir_all(&dir).map_err(|source| {
                crate::core::error::ConfigError::RootCreate { path: dir, source }
            })?;
        }
        let ns = Arc::new(Namespace::new(
            full.to_owned(),
            Arc::clone(&self.registry),
            Arc::clone(self),
        ));
        guard.insert(full.to_owned(), Arc::clone(&ns));
        #[cfg(feature = "tracing")]
        tracing::debug!(namespace = %full, "namespace created");
        Ok(ns)
    }
}

#[cfg(test)]
pub(crate) fn test_shared(root: &std::path::Path) -> Arc<NamespaceShared> {
    use crate::core::manager::{ManagerConfig, SymlinkPolicy};
    use crate::observability::NullSink;
    use crate::recovery::RecoveryPolicy;
    use core::time::Duration;

    let config = ManagerConfig {
        root: root.to_path_buf(),
        create_if_missing: true,
        cleanup_on_start: false,
        cleanup_on_shutdown: false,
        max_size: None,
        max_resources: None,
        default_ttl: Some(Duration::from_secs(3600)),
        symlink_policy: SymlinkPolicy::Reject,
        instance_id: "instance-test".to_string(),
        recovery_policy: RecoveryPolicy::default(),
        stale_instance_after: Duration::from_secs(3600),
        remove_instance_dir_on_shutdown: false,
        shutdown_wait_leases: Duration::ZERO,
    };
    let registry = Arc::new(Registry::new());
    #[cfg(feature = "filesystem")]
    let backends = Backends::new(root, "instance-test", SymlinkPolicy::Reject).unwrap();
    #[cfg(not(feature = "filesystem"))]
    let backends = Backends::new().unwrap();
    let counters = Arc::new(StatisticsCollector::new());
    let leases = crate::ownership::LeaseBook::with_counters(Arc::clone(&counters));
    Arc::new(NamespaceShared {
        config: Arc::new(config),
        registry,
        backends,
        leases,
        counters,
        sink: Arc::new(NullSink),
        namespaces: Weak::new(),
        closed: Arc::new(AtomicBool::new(false)),
        cleanup_policy: RwLock::new(None),
        custom_cleanup_policy: RwLock::new(None),
    })
}
