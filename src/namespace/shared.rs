//! Plumbing shared between the manager and its namespaces.
//!
//! The manager builds one [`NamespaceShared`] at startup and hands a
//! clone to every namespace it creates. That is how namespaces reach
//! the backends, the manager-wide configuration, the event sink and
//! the creation lock without globals or I/O in the registry.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::backend::{BackendRegistry, ResourceBackend};
use crate::core::manager::ManagerConfig;
use crate::core::registry::Registry;
use crate::core::resource::ResourceKind;
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
    /// Where lifecycle events go.
    pub(crate) sink: Arc<dyn EventSink>,
    /// Serializes resource creation so quota checks stay atomic.
    pub(crate) create_lock: Mutex<()>,
    /// Shared shutdown flag; set by `TempManager::shutdown`.
    pub(crate) closed: Arc<AtomicBool>,
}

impl NamespaceShared {
    pub(crate) fn emit(&self, event: Event) {
        self.sink.emit(&event);
    }
}

#[cfg(test)]
pub(crate) fn test_shared(root: &std::path::Path) -> Arc<NamespaceShared> {
    use crate::core::manager::{ManagerConfig, SymlinkPolicy};
    use crate::observability::NullSink;
    use crate::recovery::RecoveryPolicy;
    use std::time::Duration;

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
    };
    let registry = Arc::new(Registry::new());
    #[cfg(feature = "filesystem")]
    let backends = Backends::new(root, "instance-test", SymlinkPolicy::Reject).unwrap();
    #[cfg(not(feature = "filesystem"))]
    let backends = Backends::new().unwrap();
    Arc::new(NamespaceShared {
        config: Arc::new(config),
        registry,
        backends,
        counters: Arc::new(StatisticsCollector::new()),
        sink: Arc::new(NullSink),
        create_lock: Mutex::new(()),
        closed: Arc::new(AtomicBool::new(false)),
    })
}
