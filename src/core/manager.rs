//! The central manager: builder, namespaces, shutdown.
//!
//! A manager owns a registry of temporary resources, hands out
//! namespaces, and exposes explicit cleanup and shutdown hooks. It
//! never spawns background threads and never requires an async runtime.

use core::time::Duration;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::cleanup::{CleanupEngine, CleanupReport, SweepView};
use crate::core::error::{ConcurrencyError, ConfigError, Result};
use crate::namespace::{Backends, Namespace, NamespaceShared, validate_namespace_name};
use crate::observability::{EventSink, NullSink, Statistics, StatisticsCollector};
use crate::policy::CleanupPolicy;
#[cfg(feature = "filesystem")]
use crate::recovery::RecoveryManager;
use crate::recovery::{RecoveryPolicy, RecoveryReport};

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
///
/// Serializes as a flat object; `PathBuf` fields become strings (so paths
/// must be representable as UTF-8 to survive a roundtrip) and timestamps
/// are unix seconds/nanos. Deserialization tolerates missing fields that
/// carry a default: `create_if_missing` (true), `cleanup_on_start` /
/// `cleanup_on_shutdown` (false), `symlink_policy`, `recovery_policy` and
/// `stale_instance_after` (one hour). `root` and `instance_id` have no
/// default and are required. Unknown fields are ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ManagerConfig {
    /// Root directory all file resources live under.
    pub root: PathBuf,
    /// Create the root directory if it does not exist.
    #[cfg_attr(feature = "serde", serde(default = "default_create_if_missing"))]
    pub create_if_missing: bool,
    /// Reclaim stale instance data when the manager is built.
    #[cfg_attr(feature = "serde", serde(default))]
    pub cleanup_on_start: bool,
    /// Sweep expired resources when the manager is shut down.
    #[cfg_attr(feature = "serde", serde(default))]
    pub cleanup_on_shutdown: bool,
    /// Manager-wide quota in bytes.
    pub max_size: Option<u64>,
    /// Manager-wide resource count limit.
    pub max_resources: Option<u64>,
    /// Default TTL applied to resources unless overridden.
    pub default_ttl: Option<Duration>,
    /// Symlink handling for file operations.
    #[cfg_attr(feature = "serde", serde(default))]
    pub symlink_policy: SymlinkPolicy,
    /// Instance id for this manager.
    pub instance_id: String,
    /// How crash recovery behaves on startup.
    #[cfg_attr(feature = "serde", serde(default))]
    pub recovery_policy: RecoveryPolicy,
    /// How old a directory without a lock file must be before recovery treats it as stale.
    #[cfg_attr(feature = "serde", serde(default = "default_stale_instance_after"))]
    pub stale_instance_after: Duration,
}

#[cfg(feature = "serde")]
fn default_create_if_missing() -> bool {
    true
}

#[cfg(feature = "serde")]
fn default_stale_instance_after() -> Duration {
    crate::recovery::DEFAULT_STALE_INSTANCE_AFTER
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
    recovery_report: Option<RecoveryReport>,
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

    /// Report from the startup recovery pass, when `cleanup_on_start` was
    /// enabled. `None` otherwise, and always `None` without the
    /// `filesystem` feature.
    pub fn recovery_report(&self) -> Option<&RecoveryReport> {
        self.recovery_report.as_ref()
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
        #[cfg(feature = "tracing")]
        tracing::debug!(namespace = %name, "namespace created");
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

    /// Reclaim everything in `Expired` or `PendingCleanup` state and enforce limits.
    ///
    /// The pass expires resources whose TTL has run out (fixed or idle,
    /// per resource or per policy), applies any configured custom policy,
    /// then evicts the least valuable live resources (by priority, then
    /// least-recently-used) until namespace-level and manager-wide quotas
    /// are satisfied. Explicit by design: the application decides when
    /// this runs. Resources with active leases are never destroyed; they
    /// are deferred and counted as such in the report.
    pub fn cleanup_expired(&self) -> Result<CleanupReport> {
        let views: HashMap<String, SweepView> = {
            let namespaces = self
                .namespaces
                .lock()
                .map_err(|_| ConcurrencyError::Poisoned)?;
            namespaces
                .iter()
                .map(|(name, ns)| (name.clone(), ns.sweep_view()))
                .collect()
        };
        let report = match CleanupEngine::cleanup_manager(&self.shared, &views) {
            Ok(report) => report,
            Err(source) => {
                #[cfg(feature = "tracing")]
                tracing::error!(error = %source, "cleanup pass failed");
                return Err(source);
            }
        };
        self.counters
            .record_gc_run(report.removed, report.bytes_freed);
        Ok(report)
    }

    /// Effective manager-wide cleanup policy.
    ///
    /// Returns the configured policy if one is set, otherwise an
    /// unlimited default.
    pub fn cleanup_policy(&self) -> CleanupPolicy {
        self.shared
            .cleanup_policy
            .read()
            .ok()
            .and_then(|guard| *guard)
            .unwrap_or_default()
    }

    /// Set or replace the manager-wide cleanup policy.
    pub fn set_cleanup_policy(&self, policy: CleanupPolicy) {
        if let Ok(mut guard) = self.shared.cleanup_policy.write() {
            *guard = Some(policy);
        }
    }

    /// Remove the manager-wide cleanup policy, if any.
    pub fn clear_cleanup_policy(&self) {
        if let Ok(mut guard) = self.shared.cleanup_policy.write() {
            *guard = None;
        }
    }

    /// Set or replace the manager-wide custom cleanup policy.
    pub fn set_custom_cleanup_policy(&self, policy: Arc<dyn crate::policy::CustomCleanupPolicy>) {
        if let Ok(mut guard) = self.shared.custom_cleanup_policy.write() {
            *guard = Some(policy);
        }
    }

    /// Remove the manager-wide custom cleanup policy, if any.
    pub fn clear_custom_cleanup_policy(&self) {
        if let Ok(mut guard) = self.shared.custom_cleanup_policy.write() {
            *guard = None;
        }
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
    #[cfg_attr(feature = "tracing", tracing::instrument(
        skip(self),
        name = "shutdown",
        level = "info",
        fields(instance = %self.config.instance_id),
    ))]
    pub fn shutdown(&self) -> Result<CleanupReport> {
        self.closed.store(true, Ordering::SeqCst);
        #[cfg(feature = "tracing")]
        tracing::info!(
            cleanup_on_shutdown = self.config.cleanup_on_shutdown,
            "manager shutting down"
        );
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
        #[cfg(feature = "tracing")]
        tracing::info!(
            scanned = report.scanned,
            removed = report.removed,
            failed = report.failed,
            bytes_freed = report.bytes_freed,
            "manager shut down"
        );
        Ok(report)
    }
}

impl core::fmt::Debug for TempManager {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
    stale_instance_after: Duration,
    event_sink: Arc<dyn EventSink>,
    cleanup_policy: Option<CleanupPolicy>,
    custom_cleanup_policy: Option<Arc<dyn crate::policy::CustomCleanupPolicy>>,
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
            stale_instance_after: crate::recovery::DEFAULT_STALE_INSTANCE_AFTER,
            event_sink: Arc::new(NullSink),
            cleanup_policy: None,
            custom_cleanup_policy: None,
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
    ///
    /// [`RecoveryPolicy::OnStartup`] (the default) deletes what recovery
    /// judges certainly stale, [`RecoveryPolicy::ReportOnly`] only produces
    /// a report, and [`RecoveryPolicy::Never`] skips recovery entirely.
    /// Recovery never aborts startup: problems are emitted through the
    /// event sink and visible in [`TempManager::recovery_report`].
    pub fn recovery_policy(mut self, policy: RecoveryPolicy) -> Self {
        self.recovery_policy = policy;
        self
    }

    /// How old a directory without a lock file must be before recovery
    /// treats it as stale. Default: 1 hour.
    pub fn stale_instance_after(mut self, threshold: Duration) -> Self {
        self.stale_instance_after = threshold;
        self
    }

    /// Where structured lifecycle events are emitted. Default: [`NullSink`].
    pub fn event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = sink;
        self
    }

    /// Manager-wide cleanup policy: TTL sweep rules, aggregate quota for
    /// eviction, and the default eviction priority for new resources.
    ///
    /// Namespace-specific policies set through
    /// [`Namespace::set_cleanup_policy`] take precedence for resources of
    /// that namespace.
    ///
    /// [`Namespace::set_cleanup_policy`]: crate::Namespace::set_cleanup_policy
    pub fn cleanup_policy(mut self, policy: CleanupPolicy) -> Self {
        self.cleanup_policy = Some(policy);
        self
    }

    /// Manager-wide custom cleanup policy, consulted for resources not
    /// otherwise eligible for reclamation.
    ///
    /// Takes effect on the next cleanup pass; namespace-specific custom
    /// policies override it for their namespace.
    pub fn custom_cleanup_policy(
        mut self,
        policy: Arc<dyn crate::policy::CustomCleanupPolicy>,
    ) -> Self {
        self.custom_cleanup_policy = Some(policy);
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
            stale_instance_after: self.stale_instance_after,
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

        let recovery_report: Option<RecoveryReport>;
        #[cfg(feature = "filesystem")]
        {
            let recovery =
                RecoveryManager::new(&config.root).with_stale_after(config.stale_instance_after);
            if config.cleanup_on_start {
                // recovery must never take the manager down: errors are
                // surfaced through the report and the event sink instead
                let report = match recovery.recover(config.recovery_policy, &config.instance_id) {
                    Ok(report) => report,
                    Err(e) => RecoveryReport {
                        failed: 1,
                        failure_reasons: vec![e.to_string()],
                        ..RecoveryReport::default()
                    },
                };
                self.event_sink
                    .emit(&crate::observability::Event::RecoveryCompleted {
                        instance: config.instance_id.clone(),
                        scanned: report.scanned,
                        removed: report.removed,
                        kept: report.kept,
                        failed: report.failed,
                        bytes_freed: report.bytes_freed,
                    });
                #[cfg(feature = "tracing")]
                if report.failed > 0 {
                    tracing::warn!(
                        scanned = report.scanned,
                        removed = report.removed,
                        kept = report.kept,
                        failed = report.failed,
                        bytes_freed = report.bytes_freed,
                        "recovery completed with failures"
                    );
                } else {
                    tracing::info!(
                        scanned = report.scanned,
                        removed = report.removed,
                        kept = report.kept,
                        bytes_freed = report.bytes_freed,
                        "recovery completed"
                    );
                }
                recovery_report = Some(report);
            } else {
                recovery_report = None;
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
            cleanup_policy: std::sync::RwLock::new(self.cleanup_policy),
            custom_cleanup_policy: std::sync::RwLock::new(self.custom_cleanup_policy),
        });

        #[cfg(not(feature = "filesystem"))]
        {
            recovery_report = None;
        }

        Ok(TempManager {
            config,
            registry,
            namespaces: Mutex::new(HashMap::new()),
            closed,
            counters,
            shared,
            recovery_report,
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

    #[cfg(feature = "filesystem")]
    fn plant_dead_instance(root: &std::path::Path, name: &str) {
        let dead = root.join(name);
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(
            dead.join(".cachegirl.lock"),
            format!("version=2\npid=2147483646\ncreated=1763100000\ninstance={name}\n"),
        )
        .unwrap();
        std::fs::write(dead.join("data.bin"), vec![b'x'; 512]).unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn cleanup_on_start_reclaims_dead_instance_and_keeps_report() {
        let dir = crate::testsupport::TestDir::new();
        plant_dead_instance(dir.path(), "instance-crashed");

        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .build()
            .unwrap();
        let report = manager
            .recovery_report()
            .expect("cleanup_on_start must produce a report");
        assert_eq!(report.removed, 1, "{report}");
        assert!(report.bytes_freed >= 512);
        assert!(!dir.path().join("instance-crashed").exists());
        assert!(
            dir.path()
                .join("instance-fresh")
                .join(".cachegirl.lock")
                .exists()
        );
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn report_only_policy_builds_and_deletes_nothing() {
        let dir = crate::testsupport::TestDir::new();
        plant_dead_instance(dir.path(), "instance-crashed");

        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .recovery_policy(crate::recovery::RecoveryPolicy::ReportOnly)
            .build()
            .unwrap();
        let report = manager.recovery_report().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(dir.path().join("instance-crashed").exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn pid_reuse_does_not_fool_startup_recovery() {
        let dir = crate::testsupport::TestDir::new();
        // the lock names our pid (alive) with a lifetime token from another era:
        // that pid belongs to a different process now, so the instance is stale
        let dead = dir.path().join("instance-reused");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(
            dead.join(".cachegirl.lock"),
            format!(
                "version=2\npid={}\ncreated=1763100000\ninstance=instance-reused\nstart=somebody-elses-lifetime\n",
                std::process::id()
            ),
        )
        .unwrap();

        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .build()
            .unwrap();
        let report = manager.recovery_report().unwrap();
        assert_eq!(report.removed, 1, "{report}");
        assert!(!dead.exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn stale_directory_without_lock_needs_age() {
        let dir = crate::testsupport::TestDir::new();
        std::fs::create_dir_all(dir.path().join("instance-no-lock")).unwrap();

        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .stale_instance_after(Duration::from_secs(3600))
            .build()
            .unwrap();
        let report = manager.recovery_report().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(dir.path().join("instance-no-lock").exists());

        // backdating via a custom manager on the same root with a zero threshold;
        // the fresh instance keeps its own directory (current instance is sacred),
        // only the lock-less ghost is reclaimed
        manager.shutdown().unwrap();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .stale_instance_after(Duration::from_secs(0))
            .build()
            .unwrap();
        let report = manager.recovery_report().unwrap();
        assert_eq!(report.removed, 1);
        assert!(!dir.path().join("instance-no-lock").exists());
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "filesystem")]
    #[test]
    fn startup_recovery_never_fails_the_build() {
        // the recovery root exists, but the stale entry's directory was
        // already removed between scan and use; the build must still succeed
        let dir = crate::testsupport::TestDir::new();
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("instance-fresh")
            .cleanup_on_start(true)
            .build()
            .unwrap();
        // a fresh build over a root with no stale candidates succeeds with an
        // empty report
        let report = manager.recovery_report().unwrap();
        assert_eq!(report.failed, 0);
        manager.shutdown().unwrap();
    }
}
