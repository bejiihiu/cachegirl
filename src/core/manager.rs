//! The central manager: builder, namespaces, shutdown.
//!
//! A manager owns a registry of temporary resources, hands out
//! namespaces, and exposes explicit cleanup and shutdown hooks. It
//! never spawns background threads and never requires an async runtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::cleanup::{CleanupEngine, CleanupReport};
use crate::core::error::{ConcurrencyError, ConfigError, Result};
use crate::namespace::{Backends, Namespace, NamespaceShared, validate_namespace_name};
use crate::observability::{EventSink, NullSink, Statistics, StatisticsCollector};
#[cfg(feature = "filesystem")]
use crate::recovery::RecoveryManager;
use crate::recovery::RecoveryPolicy;

/// How the filesystem backend treats symbolic links.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SymlinkPolicy {
    /// Refuse to touch symlinked paths. Safest; the default.
    #[default]
    Reject,
    /// Allow symlinks but only within the manager's root.
    WithinRoot,
    /// Allow symlinks anywhere the running user can reach.
    Allow,
}

/// Validated manager configuration. Build one via [`TempManager::builder`].
#[derive(Clone, Debug)]
pub struct ManagerConfig {
    /// Root directory all file resources live under.
    pub root: PathBuf,
    /// Create the root directory if it does not exist.
    pub create_if_missing: bool,
    /// Reclaim stale instance data when the manager is built.
    pub cleanup_on_start: bool,
    /// Sweep expired resources when the manager is shut down.
    pub cleanup_on_shutdown: bool,
    /// Manager-wide quota in bytes.
    pub max_size: Option<u64>,
    /// Manager-wide resource count limit.
    pub max_resources: Option<u64>,
    /// Default TTL applied to resources unless overridden.
    pub default_ttl: Option<Duration>,
    /// Symlink handling for file operations.
    pub symlink_policy: SymlinkPolicy,
    /// Instance id for this manager.
    pub instance_id: String,
    /// How crash recovery behaves on startup.
    pub recovery_policy: RecoveryPolicy,
}

impl ManagerConfig {
    fn validate(&self) -> Result<()> {
        if self.instance_id.trim().is_empty()
            || !self
                .instance_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(ConfigError::InvalidInstanceId(self.instance_id.clone()).into());
        }
        if self.max_size == Some(0) {
            return Err(ConfigError::Invalid("`max_size` must be at least 1 byte".into()).into());
        }
        if self.max_resources == Some(0) {
            return Err(ConfigError::Invalid("`max_resources` must be at least 1".into()).into());
        }
        Ok(())
    }
}

/// Entry point of the library. Create with [`TempManager::builder`].
///
/// A manager can coexist with other managers in the same process; there
/// is no global state.
pub struct TempManager {
    config: ManagerConfig,
    registry: Arc<crate::core::registry::Registry>,
    namespaces: Mutex<HashMap<String, Arc<Namespace>>>,
    closed: Arc<AtomicBool>,
    counters: Arc<StatisticsCollector>,
    shared: Arc<NamespaceShared>,
}

impl TempManager {
    /// Start configuring a manager.
    pub fn builder() -> TempManagerBuilder {
        TempManagerBuilder::default()
    }

    #[cfg(feature = "filesystem")]
    fn instance_dir(&self) -> PathBuf {
        self.config.root.join(&self.config.instance_id)
    }

    /// Root directory this manager confines itself to.
    pub fn root(&self) -> &Path {
        &self.config.root
    }

    /// Instance id of this manager.
    pub fn instance_id(&self) -> &str {
        &self.config.instance_id
    }

    /// Read-only view of the validated configuration.
    pub fn config(&self) -> &ManagerConfig {
        &self.config
    }

    /// Shared registry of all managed resources.
    pub fn registry(&self) -> Arc<crate::core::registry::Registry> {
        Arc::clone(&self.registry)
    }

    /// Register a custom backend under its own name.
    ///
    /// The backend becomes reachable through [`Namespace::create_custom`] and through
    /// release routing for resources it materialized. Re-registering the same name
    /// replaces the previous backend.
    pub fn register_backend(
        &self,
        backend: Arc<dyn crate::backend::ResourceBackend>,
    ) -> Result<()> {
        self.shared
            .backends
            .custom
            .register(backend)
            .map_err(Into::into)
    }

    /// Flags whether the manager accepted the shutdown sequence.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Get or create a namespace.
    ///
    /// Namespace names must be non-empty, use `[A-Za-z0-9._-]`, and
    /// contain no path separators. Creating a namespace after shutdown
    /// fails.
    pub fn namespace(&self, name: &str) -> Result<Arc<Namespace>> {
        validate_namespace_name(name)?;
        if self.is_closed() {
            return Err(ConcurrencyError::ManagerClosed.into());
        }
        let mut namespaces = self
            .namespaces
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?;
        if let Some(ns) = namespaces.get(name) {
            return Ok(Arc::clone(ns));
        }
        #[cfg(feature = "filesystem")]
        {
            let dir = self.instance_dir().join(name);
            std::fs::create_dir_all(&dir)
                .map_err(|source| ConfigError::RootCreate { path: dir, source })?;
        }
        let ns = Arc::new(Namespace::new(
            name.to_owned(),
            Arc::clone(&self.registry),
            Arc::clone(&self.shared),
        ));
        namespaces.insert(name.to_owned(), Arc::clone(&ns));
        Ok(ns)
    }

    /// Names of all namespaces created so far.
    pub fn namespaces(&self) -> Result<Vec<String>> {
        Ok(self
            .namespaces
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?
            .keys()
            .cloned()
            .collect())
    }

    /// Reclaim all resources in `Expired` or `PendingCleanup` state.
    ///
    /// Explicit by design: the application decides when this runs.
    /// Resources with active leases are never destroyed; they are
    /// deferred and counted as such in the report.
    pub fn cleanup_expired(&self) -> Result<CleanupReport> {
        let engine = CleanupEngine::new(Arc::clone(&self.shared));
        let report = engine.cleanup()?;
        self.counters
            .record_gc_run(report.removed, report.bytes_freed);
        Ok(report)
    }

    /// Statistics snapshot for observability.
    pub fn stats(&self) -> Result<Statistics> {
        let namespaces = self
            .namespaces
            .lock()
            .map_err(|_| ConcurrencyError::Poisoned)?
            .len() as u64;
        self.counters
            .snapshot(&self.config.instance_id, namespaces, &self.registry)
    }

    /// Shut the manager down.
    ///
    /// Blocks new namespace creation, optionally sweeps expired
    /// resources, and removes this instance's lock file. Safe to call
    /// more than once.
    pub fn shutdown(&self) -> Result<CleanupReport> {
        self.closed.store(true, Ordering::SeqCst);
        let report = if self.config.cleanup_on_shutdown {
            self.cleanup_expired()?
        } else {
            CleanupReport::default()
        };
        #[cfg(feature = "filesystem")]
        {
            let recovery = RecoveryManager::new(&self.config.root);
            // lock file removal is best-effort: a crash must not fail shutdown
            let _ = recovery.remove_lock(&self.config.instance_id);
        }
        Ok(report)
    }
}

impl std::fmt::Debug for TempManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TempManager")
            .field("instance_id", &self.config.instance_id)
            .field("root", &self.config.root)
            .finish_non_exhaustive()
    }
}

/// Builder for [`TempManager`]. Finish with [`TempManagerBuilder::build`].
pub struct TempManagerBuilder {
    root: Option<PathBuf>,
    create_if_missing: bool,
    cleanup_on_start: bool,
    cleanup_on_shutdown: bool,
    max_size: Option<u64>,
    max_resources: Option<u64>,
    default_ttl: Option<Duration>,
    symlink_policy: SymlinkPolicy,
    instance_id: Option<String>,
    recovery_policy: RecoveryPolicy,
    event_sink: Arc<dyn EventSink>,
}

impl Default for TempManagerBuilder {
    fn default() -> Self {
        Self {
            root: None,
            create_if_missing: true,
            cleanup_on_start: false,
            cleanup_on_shutdown: false,
            max_size: None,
            max_resources: None,
            default_ttl: None,
            symlink_policy: SymlinkPolicy::default(),
            instance_id: None,
            recovery_policy: RecoveryPolicy::default(),
            event_sink: Arc::new(NullSink),
        }
    }
}

impl TempManagerBuilder {
    /// Root directory for all temporary data. Required.
    pub fn root(mut self, path: impl Into<PathBuf>) -> Self {
        self.root = Some(path.into());
        self
    }

    /// Create the root directory (and instance dir) when missing. Default: true.
    pub fn create_if_missing(mut self, create: bool) -> Self {
        self.create_if_missing = create;
        self
    }

    /// Reclaim stale instance data from crashed runs when the manager starts.
    pub fn cleanup_on_start(mut self, cleanup: bool) -> Self {
        self.cleanup_on_start = cleanup;
        self
    }

    /// Sweep expired resources during [`TempManager::shutdown`].
    pub fn cleanup_on_shutdown(mut self, cleanup: bool) -> Self {
        self.cleanup_on_shutdown = cleanup;
        self
    }

    /// Manager-wide quota in bytes.
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.max_size = Some(bytes);
        self
    }

    /// Manager-wide limit on the number of resources.
    pub fn max_resources(mut self, count: u64) -> Self {
        self.max_resources = Some(count);
        self
    }

    /// Default TTL for resources that do not set their own.
    pub fn default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Symlink handling policy for file operations.
    pub fn symlink_policy(mut self, policy: SymlinkPolicy) -> Self {
        self.symlink_policy = policy;
        self
    }

    /// Explicit instance id; a random one is generated when omitted.
    pub fn instance_id(mut self, id: impl Into<String>) -> Self {
        self.instance_id = Some(id.into());
        self
    }

    /// How stale instance data from crashed runs is reclaimed.
    pub fn recovery_policy(mut self, policy: RecoveryPolicy) -> Self {
        self.recovery_policy = policy;
        self
    }

    /// Where structured lifecycle events are emitted. Default: [`NullSink`].
    pub fn event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = sink;
        self
    }

    /// Validate and build the manager.
    pub fn build(self) -> Result<TempManager> {
        let root = self
            .root
            .ok_or_else(|| ConfigError::Invalid("`root` is required".into()))?;
        let instance_id = self.instance_id.unwrap_or_else(|| {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::time::{SystemTime, UNIX_EPOCH};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("instance-{nanos:x}-{tick:x}")
        });
        let config = ManagerConfig {
            root,
            create_if_missing: self.create_if_missing,
            cleanup_on_start: self.cleanup_on_start,
            cleanup_on_shutdown: self.cleanup_on_shutdown,
            max_size: self.max_size,
            max_resources: self.max_resources,
            default_ttl: self.default_ttl,
            symlink_policy: self.symlink_policy,
            instance_id,
            recovery_policy: self.recovery_policy,
        };
        config.validate()?;

        // materialize the root before canonicalizing it
        match (&config.root.exists(), config.create_if_missing) {
            (false, true) => {
                std::fs::create_dir_all(&config.root).map_err(|source| ConfigError::RootCreate {
                    path: config.root.clone(),
                    source,
                })?
            }
            (false, false) => return Err(ConfigError::RootMissing(config.root.clone()).into()),
            (true, _) => {}
        }
        let root =
            std::fs::canonicalize(&config.root).map_err(|source| ConfigError::RootCreate {
                path: config.root.clone(),
                source,
            })?;
        let config = ManagerConfig { root, ..config };

        #[cfg(feature = "filesystem")]
        {
            let recovery = RecoveryManager::new(&config.root);
            if config.cleanup_on_start {
                recovery.recover(config.recovery_policy, &config.instance_id)?;
            }
            recovery
                .write_lock(&config.instance_id)
                .map_err(|source| ConfigError::RootCreate {
                    path: config.root.join(&config.instance_id),
                    source,
                })?;
        }

        #[cfg(feature = "filesystem")]
        let backends = Backends::new(&config.root, &config.instance_id, config.symlink_policy)?;
        #[cfg(not(feature = "filesystem"))]
        let backends = Backends::new()?;

        let registry = Arc::new(crate::core::registry::Registry::new());
        let counters = Arc::new(StatisticsCollector::new());
        let closed = Arc::new(AtomicBool::new(false));
        let leases = crate::ownership::LeaseBook::with_counters(Arc::clone(&counters));
        let shared = Arc::new(NamespaceShared {
            config: Arc::new(config.clone()),
            registry: Arc::clone(&registry),
            backends,
            counters: Arc::clone(&counters),
            leases,
            sink: self.event_sink,
            create_lock: Mutex::new(()),
            closed: Arc::clone(&closed),
        });

        Ok(TempManager {
            config,
            registry,
            namespaces: Mutex::new(HashMap::new()),
            closed,
            counters,
            shared,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_root() {
        assert!(TempManager::builder().build().is_err());
    }

    #[test]
    fn missing_root_with_create_disabled_fails() {
        let dir = crate::testsupport::TestDir::new();
        let missing = dir.path().join("does-not-exist");
        let err = TempManager::builder()
            .root(&missing)
            .create_if_missing(false)
            .build();
        assert!(matches!(
            err,
            Err(crate::core::error::Error::Config(ConfigError::RootMissing(
                _
            )))
        ));
    }

    #[test]
    fn builds_and_namespaces() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-unit")
            .build()
            .unwrap();
        let ns = manager.namespace("protocol").unwrap();
        assert_eq!(ns.name(), "protocol");
        // same namespace, same object
        let again = manager.namespace("protocol").unwrap();
        assert!(Arc::ptr_eq(&ns, &again));
        assert_eq!(manager.namespaces().unwrap(), vec!["protocol".to_string()]);
        assert!(manager.namespace("bad/name").is_err());
        manager.shutdown().unwrap();
        assert!(manager.namespace("after").is_err());
    }

    #[test]
    fn shutdown_with_cleanup_on_shutdown() {
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .cleanup_on_shutdown(true)
            .build()
            .unwrap();
        let report = manager.shutdown().unwrap();
        assert_eq!(report.scanned, 0);
    }
}
