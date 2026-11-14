//! Async manager façade.

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::manager::{ManagerConfig, SymlinkPolicy, TempManager, TempManagerBuilder};
use crate::core::registry::Registry;
use crate::observability::Statistics;
use crate::recovery::{RecoveryPolicy, RecoveryReport};
use crate::{CleanupPolicy, CleanupReport};

use super::{AsyncNamespace, offload};

/// Async entry point of the library. Create with [`AsyncTempManager::builder`].
///
/// A cheap-to-clone handle to the same state the sync [`TempManager`]
/// uses. Everything here that enters the manager runs through an
/// internal offload step: on a Tokio runtime filesystem work happens on
/// the blocking pool, otherwise on the calling thread. The synchronous
/// accessors (`root`, `instance_id`, `config`, `is_closed`, `registry`,
/// `recovery_report`) borrow state directly and never touch I/O.
///
/// Cloning an `AsyncTempManager` clones an `Arc`; it does not create a
/// new manager and never allocates a fresh instance id.
pub struct AsyncTempManager {
    inner: Arc<TempManager>,
}

impl Clone for AsyncTempManager {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl core::fmt::Debug for AsyncTempManager {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AsyncTempManager")
            .field("instance_id", &self.inner.instance_id())
            .field("root", &self.inner.root())
            .finish_non_exhaustive()
    }
}

impl AsyncTempManager {
    /// Start configuring a manager.
    pub fn builder() -> AsyncTempManagerBuilder {
        AsyncTempManagerBuilder::default()
    }

    /// Wrap the sync manager behind this handle, sharing state.
    fn from_sync(inner: TempManager) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Root directory this manager confines itself to.
    pub fn root(&self) -> &Path {
        self.inner.root()
    }

    /// Instance id of this manager.
    pub fn instance_id(&self) -> &str {
        self.inner.instance_id()
    }

    /// Read-only view of the validated configuration.
    pub fn config(&self) -> &ManagerConfig {
        self.inner.config()
    }

    /// Report from the startup recovery pass, when `cleanup_on_start` was
    /// enabled. `None` otherwise, and always `None` without the
    /// `filesystem` feature.
    pub fn recovery_report(&self) -> Option<&RecoveryReport> {
        self.inner.recovery_report()
    }

    /// Shared registry of all managed resources.
    pub fn registry(&self) -> Arc<Registry> {
        self.inner.registry()
    }

    /// Flags whether the manager accepted the shutdown sequence.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Unwrap into the underlying sync manager, when this is the only
    /// handle left.
    ///
    /// Multiple clones simply hand their shared `Arc` back; nothing is
    /// ever destroyed implicitly.
    pub fn into_inner(self) -> Arc<TempManager> {
        self.inner
    }

    /// Borrow the underlying sync manager.
    pub fn as_inner(&self) -> &TempManager {
        &self.inner
    }

    /// Get or create a namespace.
    ///
    /// Creating a namespace materializes `root/<instance>/<name>` on
    /// disk, so this offloads to the blocking pool under Tokio.
    ///
    /// # Errors
    ///
    /// Fails on an invalid namespace name or when the manager has been
    /// shut down.
    pub async fn namespace(&self, name: &str) -> Result<AsyncNamespace> {
        let inner = Arc::clone(&self.inner);
        let name = name.to_owned();
        let ns = offload(move || inner.namespace(&name)).await?;
        Ok(AsyncNamespace::new(ns))
    }

    /// Names of all namespaces created so far.
    pub async fn namespaces(&self) -> Result<Vec<String>> {
        self.inner.namespaces()
    }

    /// Register a custom backend under its own name.
    ///
    /// Re-registering the same name replaces the previous backend.
    pub async fn register_backend(
        &self,
        backend: Arc<dyn crate::backend::ResourceBackend>,
    ) -> Result<()> {
        self.inner.register_backend(backend)
    }

    /// Reclaim everything in `Expired` or `PendingCleanup` state and enforce limits.
    ///
    /// Same semantics as [`TempManager::cleanup_expired`]: TTL sweep,
    /// custom policies, quota eviction, lease-gated destruction. Runs on
    /// the blocking pool under Tokio.
    pub async fn cleanup_expired(&self) -> Result<CleanupReport> {
        let inner = Arc::clone(&self.inner);
        offload(move || inner.cleanup_expired()).await
    }

    /// Effective manager-wide cleanup policy.
    ///
    /// Returns the configured policy if one is set, otherwise an
    /// unlimited default.
    pub async fn cleanup_policy(&self) -> CleanupPolicy {
        self.inner.cleanup_policy()
    }

    /// Set or replace the manager-wide cleanup policy.
    pub async fn set_cleanup_policy(&self, policy: CleanupPolicy) {
        self.inner.set_cleanup_policy(policy)
    }

    /// Remove the manager-wide cleanup policy, if any.
    pub async fn clear_cleanup_policy(&self) {
        self.inner.clear_cleanup_policy()
    }

    /// Set or replace the manager-wide custom cleanup policy.
    pub async fn set_custom_cleanup_policy(
        &self,
        policy: Arc<dyn crate::policy::CustomCleanupPolicy>,
    ) {
        self.inner.set_custom_cleanup_policy(policy)
    }

    /// Remove the manager-wide custom cleanup policy, if any.
    pub async fn clear_custom_cleanup_policy(&self) {
        self.inner.clear_custom_cleanup_policy()
    }

    /// Statistics snapshot for observability.
    pub async fn stats(&self) -> Result<Statistics> {
        self.inner.stats()
    }

    /// Shut the manager down.
    ///
    /// Blocks new operations, waits for active leases when
    /// `shutdown_wait_leases` is set, optionally sweeps expired resources
    /// (`cleanup_on_shutdown`), removes this instance's lock file, and
    /// deletes this instance's directory when
    /// `remove_instance_dir_on_shutdown` is set. Safe to call more than
    /// once. Runs on the blocking pool under Tokio.
    pub async fn shutdown(&self) -> Result<CleanupReport> {
        let inner = Arc::clone(&self.inner);
        offload(move || inner.shutdown()).await
    }
}

/// Builder for [`AsyncTempManager`]. Finish with [`AsyncTempManagerBuilder::build`].
///
/// Accepts the same configuration as the synchronous builder; only
/// `build` is async, because startup recovery probes processes and
/// reclaims stale instance directories.
#[derive(Default)]
pub struct AsyncTempManagerBuilder {
    inner: TempManagerBuilder,
}

impl AsyncTempManagerBuilder {
    /// Root directory for all temporary data. Required.
    pub fn root(mut self, path: impl Into<PathBuf>) -> Self {
        self.inner = self.inner.root(path);
        self
    }

    /// Create the root directory (and instance dir) when missing. Default: true.
    pub fn create_if_missing(mut self, create: bool) -> Self {
        self.inner = self.inner.create_if_missing(create);
        self
    }

    /// Reclaim stale instance data from crashed runs when the manager starts.
    pub fn cleanup_on_start(mut self, cleanup: bool) -> Self {
        self.inner = self.inner.cleanup_on_start(cleanup);
        self
    }

    /// Delete this instance's whole directory (`root/<instance>`) on shutdown.
    /// Default: false. The removal is confined to the canonicalized root;
    /// anything that does not canonically resolve strictly inside it is refused.
    pub fn remove_instance_dir_on_shutdown(mut self, remove: bool) -> Self {
        self.inner = self.inner.remove_instance_dir_on_shutdown(remove);
        self
    }

    /// How long shutdown waits for active leases before sweeping.
    /// Default: zero — shutdown never waits. Leases are how the library
    /// tracks operations still in flight; operations that must be seen
    /// by shutdown must hold a lease.
    pub fn shutdown_wait_leases(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.shutdown_wait_leases(timeout);
        self
    }

    /// Sweep expired resources during [`AsyncTempManager::shutdown`].
    pub fn cleanup_on_shutdown(mut self, cleanup: bool) -> Self {
        self.inner = self.inner.cleanup_on_shutdown(cleanup);
        self
    }

    /// Manager-wide quota in bytes.
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.inner = self.inner.max_size(bytes);
        self
    }

    /// Manager-wide limit on the number of resources.
    pub fn max_resources(mut self, count: u64) -> Self {
        self.inner = self.inner.max_resources(count);
        self
    }

    /// Default TTL for resources that do not set their own.
    pub fn default_ttl(mut self, ttl: Duration) -> Self {
        self.inner = self.inner.default_ttl(ttl);
        self
    }

    /// Symlink handling policy for file operations.
    pub fn symlink_policy(mut self, policy: SymlinkPolicy) -> Self {
        self.inner = self.inner.symlink_policy(policy);
        self
    }

    /// Explicit instance id; a random one is generated when omitted.
    pub fn instance_id(mut self, id: impl Into<String>) -> Self {
        self.inner = self.inner.instance_id(id);
        self
    }

    /// How stale instance data from crashed runs is reclaimed.
    ///
    /// Recovery never aborts startup: problems are emitted through the
    /// event sink and visible in [`AsyncTempManager::recovery_report`].
    pub fn recovery_policy(mut self, policy: RecoveryPolicy) -> Self {
        self.inner = self.inner.recovery_policy(policy);
        self
    }

    /// How old a directory without a lock file must be before recovery
    /// treats it as stale. Default: 1 hour.
    pub fn stale_instance_after(mut self, threshold: Duration) -> Self {
        self.inner = self.inner.stale_instance_after(threshold);
        self
    }

    /// Where structured lifecycle events are emitted. Default: [`crate::NullSink`].
    pub fn event_sink(mut self, sink: Arc<dyn crate::observability::EventSink>) -> Self {
        self.inner = self.inner.event_sink(sink);
        self
    }

    /// Manager-wide cleanup policy: TTL sweep rules, aggregate quota for
    /// eviction, and the default eviction priority for new resources.
    ///
    /// Namespace-specific policies set through
    /// [`AsyncNamespace::set_cleanup_policy`](crate::AsyncNamespace::set_cleanup_policy)
    /// take precedence for resources of that namespace.
    pub fn cleanup_policy(mut self, policy: CleanupPolicy) -> Self {
        self.inner = self.inner.cleanup_policy(policy);
        self
    }

    /// Manager-wide custom cleanup policy, consulted for resources not
    /// otherwise eligible for reclamation.
    pub fn custom_cleanup_policy(
        mut self,
        policy: Arc<dyn crate::policy::CustomCleanupPolicy>,
    ) -> Self {
        self.inner = self.inner.custom_cleanup_policy(policy);
        self
    }

    /// Validate and build the manager.
    ///
    /// Startup recovery (pid probing, stale-instance reclamation) happens
    /// here, so under Tokio this runs on the blocking pool.
    pub async fn build(self) -> Result<AsyncTempManager> {
        let inner = self.inner;
        let manager = offload(move || inner.build()).await?;
        Ok(AsyncTempManager::from_sync(manager))
    }
}
