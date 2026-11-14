//! Registry-slot reservation for a resource being created.
//!
//! Creating a resource under a quota needs three steps that must not be
//! reordered: check the quota, hand the backend the request, register
//! what came back. Doing the check and the register in separate steps
//! lets two creators both pass the check and then both register —
//! quota overshoot. Registering an `Uninitialized` placeholder under
//! the registry lock makes the check and the insertion one indivisible
//! step; the placeholder is then either published (`publish`) or rolled
//! back (`Drop`).
//!
//! Design notes:
//! - the reservation is what stops concurrent creators in *different*
//!   namespaces from overshooting the manager-wide limit, so no global
//!   creation mutex is needed anywhere;
//! - only one critical section per create touches the registry, and it
//!   is in-memory bookkeeping — backend work (file I/O, allocation)
//!   happens outside it;
//! - a panic anywhere between reservation and publish rolls the
//!   reservation back through `Drop`, so a failing backend never leaks
//!   accounting slots.

use std::sync::Arc;

use crate::core::error::{QuotaError, RegistryError};
use crate::core::resource::{ResourceId, ResourceMeta};
use crate::namespace::NamespaceShared;

/// Why a reservation was not granted.
#[derive(Debug)]
pub(crate) enum ReserveFailure {
    /// A manager-wide or namespace quota would be exceeded.
    Quota {
        /// `"manager"` or the full path of the namespace that refused.
        scope: String,
        /// The concrete limit violation.
        source: QuotaError,
    },
    /// The registry itself failed (poisoned or a duplicate id).
    Registry(RegistryError),
}

impl From<crate::core::registry::ReserveFailure> for ReserveFailure {
    fn from(failure: crate::core::registry::ReserveFailure) -> Self {
        use crate::core::registry::ReserveFailure as Inner;
        match failure {
            Inner::Quota(failure) => ReserveFailure::Quota {
                scope: failure.scope,
                source: failure.source,
            },
            Inner::Registry(source) => ReserveFailure::Registry(source),
        }
    }
}

/// A reserved registry slot for a not-yet-materialized resource.
///
/// Dropping a reservation that was never published removes the
/// placeholder from the registry and refunds the quota. `publish`
/// consumes this guard and disarms it.
pub(crate) struct Reservation {
    shared: Arc<NamespaceShared>,
    /// `None` once published or once the reservation has been consumed.
    id: Option<ResourceId>,
}

impl Reservation {
    /// Insert the placeholder into the registry, enforcing every quota.
    pub(crate) fn reserve(
        shared: Arc<NamespaceShared>,
        meta: ResourceMeta,
        spec: crate::core::registry::QuotaReservation,
    ) -> Result<Self, ReserveFailure> {
        let id = meta.id.clone();
        shared.registry.reserve(meta, &spec)?;
        Ok(Self {
            shared,
            id: Some(id),
        })
    }

    /// The id the resource will be published under.
    pub(crate) fn id(&self) -> &ResourceId {
        self.id
            .as_ref()
            .expect("an unpublished reservation carries its id")
    }

    /// Turn the reservation into a live `Created` resource.
    ///
    /// Takes `meta` by reference so the caller still owns it when
    /// publication fails (it needs it to release the physical side).
    /// On success the guard is disarmed; on failure the placeholder
    /// stays armed for `Drop`, which refunds the slot.
    pub(crate) fn publish(mut self, meta: &ResourceMeta) -> Result<(), crate::core::error::Error> {
        debug_assert_eq!(Some(&meta.id), self.id.as_ref());
        self.shared.registry.publish(meta)?;
        self.id = None; // disarm drop: the record is now live
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // a record that never became visible must not consume quota
            let _ = self.shared.registry.remove(&id);
        }
    }
}

impl From<ReserveFailure> for crate::core::error::Error {
    fn from(failure: ReserveFailure) -> Self {
        match failure {
            ReserveFailure::Quota { source, .. } => source.into(),
            ReserveFailure::Registry(source) => source.into(),
        }
    }
}
