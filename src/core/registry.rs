//! In-memory registry of managed resources.
//!
//! The registry is the single source of truth about what exists, who
//! owns it, and which lifecycle state it is in. It is thread-safe and
//! deliberately free of any I/O so it stays cheap to use anywhere.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::error::RegistryError;
use crate::core::lifecycle::ResourceState;
use crate::core::resource::{ResourceId, ResourceMeta};
use crate::namespace::NamespaceQuota;

/// Registry state: the map plus the aggregates maintained under the
/// same lock.
///
/// The aggregates are what quota checks consume, so they must always
/// be consistent with the map — they are updated in the same critical
/// section as every map mutation.
#[derive(Default)]
struct State {
    map: HashMap<ResourceId, ResourceMeta>,
    /// (bytes, count) summed over the whole registry.
    total: (u64, u64),
    /// (bytes, count) per namespace, keyed by the full namespace path.
    per_ns: HashMap<String, (u64, u64)>,
}

fn signed(current: u64, delta: i128) -> u64 {
    let next = current as i128 + delta;
    if next <= 0 {
        0
    } else if next > u64::MAX as i128 {
        u64::MAX
    } else {
        next as u64
    }
}

impl State {
    fn add(&mut self, meta: &ResourceMeta) {
        self.total.0 = signed(self.total.0, meta.size_bytes as i128);
        self.total.1 = signed(self.total.1, 1);
        let ns = self.per_ns.entry(meta.namespace.clone()).or_insert((0, 0));
        ns.0 = signed(ns.0, meta.size_bytes as i128);
        ns.1 = signed(ns.1, 1);
    }

    fn subtract(&mut self, meta: &ResourceMeta) {
        self.total.0 = signed(self.total.0, -(meta.size_bytes as i128));
        self.total.1 = signed(self.total.1, -1);
        if let Some(ns) = self.per_ns.get_mut(&meta.namespace) {
            ns.0 = signed(ns.0, -(meta.size_bytes as i128));
            ns.1 = signed(ns.1, -1);
            if ns.0 == 0 && ns.1 == 0 {
                self.per_ns.remove(&meta.namespace);
            }
        }
    }

    fn adjust_bytes(&mut self, namespace: &str, from: u64, to: u64) {
        if from == to {
            return;
        }
        let delta = to as i128 - from as i128;
        self.total.0 = signed(self.total.0, delta);
        if let Some(ns) = self.per_ns.get_mut(namespace) {
            ns.0 = signed(ns.0, delta);
        }
    }
}

/// Limits for a not-yet-materialized resource, resolved by the caller.
///
/// Checked atomically against the registry's internal totals when the
/// reservation goes in. `scopes` carries the namespace being created
/// into plus every ancestor, root first; subtree totals (the namespace
/// itself and all registered descendants) are what a limit is measured
/// against, so a parent's quota genuinely caps the whole subtree.
#[derive(Clone, Default)]
pub(crate) struct QuotaReservation {
    /// Manager-wide limits (`max_size`, `max_resources`).
    pub(crate) manager: NamespaceQuota,
    /// (namespace path, its quota); the sum must hold for every entry.
    pub(crate) scopes: Vec<(String, NamespaceQuota)>,
}

/// Which scope rejected a reservation, for observability.
pub(crate) struct QuotaFailure {
    /// `"manager"` or the full namespace path that hit the limit.
    pub(crate) scope: String,
    /// The concrete limit violation.
    pub(crate) source: crate::core::error::QuotaError,
}

/// Why a reservation could not be made.
pub(crate) enum ReserveFailure {
    /// A manager-wide or namespace quota would be exceeded.
    Quota(QuotaFailure),
    /// The registry itself failed (poisoned or a duplicate id).
    Registry(RegistryError),
}

impl From<ReserveFailure> for crate::core::error::Error {
    fn from(failure: ReserveFailure) -> Self {
        match failure {
            ReserveFailure::Quota(failure) => failure.source.into(),
            ReserveFailure::Registry(source) => source.into(),
        }
    }
}

fn check_scope(
    quota: NamespaceQuota,
    scope: &str,
    current: (u64, u64),
    added_bytes: u64,
) -> Result<(), QuotaFailure> {
    quota
        .check(scope, current.0, current.1, added_bytes)
        .map_err(|source| QuotaFailure {
            scope: scope.to_owned(),
            source,
        })
}

fn subtree_sums(per_ns: &HashMap<String, (u64, u64)>, prefix: &str) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut count = 0u64;
    for (ns, &(b, c)) in per_ns {
        if crate::namespace::namespace_is_subtree(ns, prefix) {
            bytes = bytes.saturating_add(b);
            count = count.saturating_add(c);
        }
    }
    (bytes, count)
}

/// Shared, thread-safe registry of [`ResourceMeta`] records.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<State>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new resource. Fails if the id is already present.
    pub fn register(&self, meta: ResourceMeta) -> Result<(), RegistryError> {
        let mut state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        if state.map.contains_key(&meta.id) {
            return Err(RegistryError::AlreadyRegistered(meta.id));
        }
        state.add(&meta);
        state.map.insert(meta.id.clone(), meta);
        Ok(())
    }

    /// Fetch a snapshot of a resource's metadata.
    pub fn get(&self, id: &ResourceId) -> Result<ResourceMeta, RegistryError> {
        let state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        state
            .map
            .get(id)
            .cloned()
            .ok_or_else(|| RegistryError::NotFound(id.clone()))
    }

    /// Apply a mutation to a registered resource.
    pub fn update(
        &self,
        id: &ResourceId,
        f: impl FnOnce(&mut ResourceMeta),
    ) -> Result<ResourceMeta, RegistryError> {
        let mut state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let before = state
            .map
            .get(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?
            .size_bytes;
        {
            let meta = state
                .map
                .get_mut(id)
                .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
            f(meta);
        }
        let (namespace, after) = {
            let meta = state
                .map
                .get(id)
                .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
            (meta.namespace.clone(), meta.size_bytes)
        };
        state.adjust_bytes(&namespace, before, after);
        Ok(state.map.get(id).cloned().unwrap())
    }

    /// Move a resource to a new lifecycle state, validating the transition.
    pub fn set_state(
        &self,
        id: &ResourceId,
        next: ResourceState,
    ) -> Result<ResourceState, RegistryError> {
        let mut state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let meta = state
            .map
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
        let from = meta.state;
        let to = from.try_transition(next)?;
        meta.state = to;
        Ok(to)
    }

    /// Remove a resource from the registry and return its last record.
    pub fn remove(&self, id: &ResourceId) -> Result<ResourceMeta, RegistryError> {
        let mut state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let meta = state
            .map
            .remove(id)
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
        state.subtract(&meta);
        Ok(meta)
    }

    /// Number of registered resources.
    pub fn len(&self) -> Result<usize, RegistryError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .map
            .len())
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> Result<bool, RegistryError> {
        Ok(self.len()? == 0)
    }

    /// Snapshot every registered resource.
    pub fn snapshot(&self) -> Result<Vec<ResourceMeta>, RegistryError> {
        let state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        Ok(state.map.values().cloned().collect())
    }

    /// Snapshot resources in a single namespace.
    pub fn snapshot_namespace(&self, namespace: &str) -> Result<Vec<ResourceMeta>, RegistryError> {
        Ok(self
            .snapshot()?
            .into_iter()
            .filter(|m| m.namespace == namespace)
            .collect())
    }

    /// Snapshot this namespace and all registered descendants.
    pub(crate) fn snapshot_subtree(
        &self,
        namespace: &str,
    ) -> Result<Vec<ResourceMeta>, RegistryError> {
        Ok(self
            .snapshot()?
            .into_iter()
            .filter(|m| crate::namespace::namespace_is_subtree(&m.namespace, namespace))
            .collect())
    }

    /// Total tracked size across all resources, in bytes.
    pub fn total_bytes(&self) -> Result<u64, RegistryError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .total
            .0)
    }

    /// Atomically check the given quotas and insert `meta` as a
    /// `Uninitialized` reservation.
    ///
    /// The check covers the manager-wide limit and every scope in
    /// `quotas` (the creating namespace and its ancestors), measuring
    /// subtree totals. Either everything is inserted and returned, or
    /// nothing changes.
    pub(crate) fn reserve(
        &self,
        meta: ResourceMeta,
        quotas: &QuotaReservation,
    ) -> Result<(), ReserveFailure> {
        if meta.state != ResourceState::Uninitialized {
            // reservations are always placeholders; that is what makes
            // the refunded slot unambiguous on failure
            return Err(ReserveFailure::Registry(RegistryError::InvalidTransition {
                from: meta.state,
                to: ResourceState::Created,
            }));
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| ReserveFailure::Registry(RegistryError::Poisoned))?;
        if state.map.contains_key(&meta.id) {
            return Err(ReserveFailure::Registry(RegistryError::AlreadyRegistered(
                meta.id,
            )));
        }
        let added = meta.size_bytes;
        check_scope(quotas.manager, "manager", state.total, added)
            .map_err(ReserveFailure::Quota)?;
        for (scope, quota) in &quotas.scopes {
            let sums = subtree_sums(&state.per_ns, scope);
            check_scope(*quota, scope, sums, added).map_err(ReserveFailure::Quota)?;
        }
        state.add(&meta);
        state.map.insert(meta.id.clone(), meta);
        Ok(())
    }

    /// Turn a reservation into a live record, without consuming `meta`.
    ///
    /// The record must still be in `Uninitialized` state and carry the
    /// id minted at reservation time. The size moves from the reserved
    /// hint to the backend's real number and the lifecycle moves to
    /// `Created` — all under one lock.
    pub(crate) fn publish(&self, meta: &ResourceMeta) -> Result<ResourceMeta, RegistryError> {
        let mut state = self.inner.lock().map_err(|_| RegistryError::Poisoned)?;
        let (from, before, ns) = {
            let old = state
                .map
                .get(&meta.id)
                .ok_or_else(|| RegistryError::NotFound(meta.id.clone()))?;
            (old.state, old.size_bytes, old.namespace.clone())
        };
        if from != ResourceState::Uninitialized {
            return Err(RegistryError::InvalidTransition {
                from,
                to: ResourceState::Created,
            });
        }
        state.adjust_bytes(&ns, before, meta.size_bytes);
        let mut stored = meta.clone();
        stored.state = ResourceState::Created;
        let old = state
            .map
            .get_mut(&meta.id)
            .ok_or_else(|| RegistryError::NotFound(meta.id.clone()))?;
        *old = stored.clone();
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::ResourceKind;

    fn meta(ns: &str) -> ResourceMeta {
        ResourceMeta::new("test", ns, ResourceKind::File)
    }

    #[test]
    fn register_get_remove_roundtrip() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        assert_eq!(reg.len().unwrap(), 1);
        let got = reg.get(&id).unwrap();
        assert_eq!(got.namespace, "a");
        let removed = reg.remove(&id).unwrap();
        assert_eq!(removed.id, id);
        assert!(reg.is_empty().unwrap());
    }

    #[test]
    fn removing_unknown_resource_fails() {
        let reg = Registry::new();
        let id = ResourceId::new();
        assert!(matches!(reg.remove(&id), Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn invalid_state_transition_keeps_old_state() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        assert!(reg.set_state(&id, ResourceState::Destroyed).is_err());
        let got = reg.get(&id).unwrap();
        assert_eq!(got.state, ResourceState::Created);
    }

    #[test]
    fn registering_the_same_id_twice_is_rejected() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let mut dup = meta("b");
        dup.id = id.clone();
        assert!(matches!(
            reg.register(dup),
            Err(RegistryError::AlreadyRegistered(_))
        ));
        // the original record must survive the rejected registration
        assert_eq!(reg.get(&id).unwrap().namespace, "a");
        assert_eq!(reg.len().unwrap(), 1);
    }

    #[test]
    fn missing_ids_surface_not_found_everywhere() {
        let reg = Registry::new();
        let id = ResourceId::new();
        assert!(matches!(reg.get(&id), Err(RegistryError::NotFound(_))));
        assert!(matches!(reg.remove(&id), Err(RegistryError::NotFound(_))));
        assert!(matches!(
            reg.update(&id, |m| m.size_bytes = 1),
            Err(RegistryError::NotFound(_))
        ));
        assert!(matches!(
            reg.set_state(&id, ResourceState::Active),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn update_applies_the_mutation_and_returns_the_new_snapshot() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let updated = reg.update(&id, |m| m.size_bytes += 7).unwrap();
        assert_eq!(updated.size_bytes, 7);
        assert_eq!(reg.get(&id).unwrap().size_bytes, 7);
    }

    #[test]
    fn snapshot_hands_out_copies_not_aliases() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        let mut got = reg.snapshot().unwrap();
        got[0].name = "tampered".into();
        // the registry must not observe mutations on handed-out records
        assert_eq!(reg.get(&id).unwrap().name, "test");
    }

    #[test]
    fn snapshot_namespace_filters_and_total_bytes_sums() {
        let reg = Registry::new();
        for (ns, bytes) in [("a", 10), ("a", 15), ("b", 100)] {
            let mut m = meta(ns);
            m.size_bytes = bytes;
            reg.register(m).unwrap();
        }
        let a = reg.snapshot_namespace("a").unwrap();
        assert_eq!(a.len(), 2);
        let b = reg.snapshot_namespace("b").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(reg.total_bytes().unwrap(), 125);
    }

    #[test]
    fn panicking_update_poisons_the_registry() {
        let reg = Registry::new();
        let m = meta("a");
        let id = m.id.clone();
        reg.register(m).unwrap();
        // a panicking closure while the registry lock is held poisons it,
        // and every operation after that must fail with Poisoned
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reg.update(&id, |_| panic!("boom while holding the lock"));
        }));
        assert!(matches!(reg.get(&id), Err(RegistryError::Poisoned)));
        assert!(matches!(
            reg.register(meta("b")),
            Err(RegistryError::Poisoned)
        ));
        assert!(matches!(reg.len(), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.remove(&id), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.snapshot(), Err(RegistryError::Poisoned)));
        assert!(matches!(reg.total_bytes(), Err(RegistryError::Poisoned)));
    }

    #[test]
    fn concurrent_registrations_and_increments_land_exactly_once() {
        use std::sync::Arc;
        let reg = Arc::new(Registry::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    reg.register(meta("shared")).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.len().unwrap(), 200);

        // concurrent increments through the mutation path must not lose updates
        let target = reg.snapshot().unwrap().into_iter().next().unwrap().id;
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    reg.update(&target, |m| m.size_bytes += 1).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.get(&target).unwrap().size_bytes, 200);
    }
}
