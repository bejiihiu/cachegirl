//! Internal glue between concrete resource handles, leases and backends.
//!
//! Everything here is `pub(crate)`: the public surface lives on the
//! resource types themselves (`resource.acquire()`, RAII drop) and on
//! `TempManager::cleanup_expired`.

use std::fmt::Debug;
use std::sync::Arc;

use crate::core::error::{BackendError, Error, RegistryError};
use crate::core::lifecycle::ResourceState;
use crate::core::resource::{Ownership, ResourceId, ResourceMeta};
use crate::namespace::NamespaceShared;
use crate::observability::Event;

/// Internal state carried by every concrete resource handle.
///
/// Cheap to clone (an `Arc`), and invisible in the public API. It is
/// how a handle finds its manager-wide lease book, registry and
/// backends.
///
/// Destruction is driven from `Drop for OwnershipCore` itself: when the
/// last handle clone lets go, the last `Arc<OwnershipCore>` drops too,
/// and that is the one and only place the resource gets destroyed.
/// Arc's refcounting makes "last drop" race-free, unlike an explicit
/// `strong_count` check in the handles.
pub(crate) struct OwnershipCore {
    shared: Arc<NamespaceShared>,
    id: ResourceId,
    mode: Ownership,
}

/// What happened to a resource during a destruction attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DestroyOutcome {
    /// Physically destroyed and unregistered.
    Destroyed,
    /// Nothing to do; someone else already removed it.
    AlreadyGone,
    /// Active leases held; the record moved to `PendingCleanup`.
    Deferred,
}

impl Drop for OwnershipCore {
    fn drop(&mut self) {
        if !self.mode.drop_destroys() {
            return;
        }
        // a misbehaving event sink must not take the process down with it
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = destroy_resource(&self.shared, &self.id);
        }));
    }
}

impl Debug for OwnershipCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnershipCore")
            .field("id", &self.id)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl OwnershipCore {
    pub(crate) fn new(shared: Arc<NamespaceShared>, id: ResourceId, mode: Ownership) -> Self {
        Self { shared, id, mode }
    }

    /// Acquire a lease on the underlying resource.
    pub(crate) fn acquire_lease(
        &self,
    ) -> Result<crate::ownership::Lease, crate::core::error::ConcurrencyError> {
        self.shared.leases.acquire(&self.id)
    }
}

/// Releases the destruction seal on every exit path, panics included.
struct SealGuard<'a> {
    shared: &'a NamespaceShared,
    id: &'a ResourceId,
}

impl Drop for SealGuard<'_> {
    fn drop(&mut self) {
        self.shared.leases.unseal(self.id);
    }
}

/// The single destruction path used by `release`, cleanup passes and drops.
///
/// Exactly-once semantics: the first thing done is sealing the id in the
/// lease book (fails while leases are held, without mutating anything),
/// then the registry record is claimed with `remove`. Whoever loses that
/// race observes `AlreadyGone` instead of double-releasing.
///
/// Callers are expected to have done their own access checks first
/// (e.g. namespace scoping in `Namespace::release`).
pub(crate) fn destroy_resource(
    shared: &Arc<NamespaceShared>,
    id: &ResourceId,
) -> Result<DestroyOutcome, Error> {
    if !shared.leases.try_seal(id) {
        defer_resource(shared, id);
        return Ok(DestroyOutcome::Deferred);
    }
    // unseals on every exit, including panics escaping a sink
    let _guard = SealGuard { shared, id };
    let meta = match shared.registry.remove(id) {
        Ok(meta) => meta,
        Err(RegistryError::NotFound(_)) => {
            return Ok(DestroyOutcome::AlreadyGone);
        }
        Err(source) => {
            return Err(source.into());
        }
    };
    match destroy_claimed(shared, &meta) {
        Ok(()) => Ok(DestroyOutcome::Destroyed),
        Err(source) => {
            // physical side failed; put a marked record back so a later
            // pass can retry instead of leaking the resource silently
            let mut failed = meta;
            failed.state = ResourceState::FailedCleanup;
            let _ = shared.registry.register(failed);
            Err(source)
        }
    }
}

fn destroy_claimed(shared: &Arc<NamespaceShared>, meta: &ResourceMeta) -> Result<(), Error> {
    let backend_name = meta
        .metadata
        .get("backend")
        .cloned()
        .or_else(|| crate::namespace::Backends::name_for_kind(meta.kind).map(str::to_owned))
        .ok_or_else(|| BackendError::Unavailable {
            backend: "unknown".to_string(),
            reason: "the resource record does not name the backend that created it".to_string(),
        })?;
    let backend =
        shared
            .backends
            .lookup(&backend_name)
            .ok_or_else(|| BackendError::Unavailable {
                backend: backend_name.clone(),
                reason: "the backend required to release this resource is not available"
                    .to_string(),
            })?;
    backend.release(meta)?;

    #[cfg(feature = "cache")]
    if meta.kind == crate::core::resource::ResourceKind::Cache {
        if let Some(key) = meta.metadata.get("cache_key") {
            shared.backends.cache_store.remove(&meta.namespace, key);
        }
    }

    shared.counters.record_release(1, meta.size_bytes);
    shared.emit(Event::ResourceReleased {
        instance: shared.config.instance_id.clone(),
        id: meta.id.clone(),
        bytes_freed: meta.size_bytes,
    });
    Ok(())
}

/// Mark the resource `PendingCleanup` when leases keep it alive.
fn defer_resource(shared: &Arc<NamespaceShared>, id: &ResourceId) {
    // already pending is fine, poisoned/moved-on stays as-is
    let _ = shared.registry.set_state(id, ResourceState::PendingCleanup);
}
