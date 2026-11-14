//! The cleanup pass itself: find reclaimable resources, release them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crate::cleanup::MAX_CLEANUP_ATTEMPTS;
use crate::cleanup::report::CleanupReport;
use crate::core::error::Error;
use crate::core::lifecycle::ResourceState;
use crate::core::registry::Registry;
use crate::core::resource::ResourceMeta;
use crate::namespace::{NamespaceQuota, NamespaceShared};
use crate::observability::metrics_support;
use crate::ownership::{DestroyOutcome, destroy_resource};
use crate::policy::{CleanupContext, CleanupScope, CustomCleanupPolicy, PolicyDecision, Ttl};

/// Per-namespace inputs the engine needs to sweep and evict honestly.
///
/// Built by the caller (the manager or a namespace) because policy and
/// quota live on those objects, not on the shared plumbing.
#[derive(Clone)]
pub(crate) struct SweepView {
    /// Effective policy: namespace-specific if set, else the manager-wide one.
    pub(crate) policy: crate::policy::CleanupPolicy,
    /// Effective custom policy: namespace-specific if set, else manager-wide.
    pub(crate) custom: Option<Arc<dyn CustomCleanupPolicy>>,
    /// The namespace's own quota (from `Namespace::set_quota`).
    pub(crate) quota: NamespaceQuota,
}

/// Why a destruction (or deferral) is happening; internal accounting tag.
#[derive(Clone, Copy)]
enum Tag {
    TtlFixed,
    TtlIdle,
    Eligible,
    Policy,
    SizeEviction,
    CountEviction,
}

impl Tag {
    fn charge(self, report: &mut CleanupReport) {
        match self {
            Tag::TtlFixed => report.reasons.ttl_fixed += 1,
            Tag::TtlIdle => report.reasons.ttl_idle += 1,
            Tag::Eligible => report.reasons.eligible += 1,
            Tag::Policy => report.reasons.policy += 1,
            Tag::SizeEviction => report.reasons.size_eviction += 1,
            Tag::CountEviction => report.reasons.count_eviction += 1,
        }
    }

    #[cfg(feature = "tracing")]
    fn reason(self) -> &'static str {
        match self {
            Tag::TtlFixed => "ttl_fixed",
            Tag::TtlIdle => "ttl_idle",
            Tag::Eligible => "eligible",
            Tag::Policy => "policy",
            Tag::SizeEviction => "size_eviction",
            Tag::CountEviction => "count_eviction",
        }
    }
}

/// Stable label for the metrics pipeline.
fn scope_str(scope: &CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Manager => "manager",
        CleanupScope::Namespace(_) => "namespace",
    }
}

/// Scans the registry, reclaims what the policies say is done.
///
/// Operates on the registry plus the manager's backends. The pass has
/// three phases: a sweep that expires TTL-bound and policy-marked
/// resources, per-namespace quota eviction, and manager-wide quota
/// eviction. Eviction orders victims by priority (lowest first) and
/// then by least-recent access. The registry record is claimed with
/// `remove`, so a concurrent release wins exactly one of the two calls.
/// There is no background garbage collector — call this whenever it
/// makes sense.
///
/// **Leases win over cleanup.** A resource with at least one active
/// lease is never destroyed: it is moved to `PendingCleanup` (if it is
/// not there already) and counted as `deferred` in the report. It is
/// reclaimed once the last lease is gone.
pub struct CleanupEngine {
    shared: Arc<NamespaceShared>,
}

impl CleanupEngine {
    // construction goes through the manager; the engine needs the whole plumbing
    pub(crate) fn new(shared: Arc<NamespaceShared>) -> Self {
        Self { shared }
    }

    /// Reclaim everything in `Expired` or `PendingCleanup` state.
    ///
    /// Manager-wide sweep without namespace views: the manager-wide
    /// policy and limits (from the configuration and `TempManager`)
    /// are enforced, per-namespace policies and quotas are not. Prefer
    /// [`TempManager::cleanup_expired`] which carries the views.
    ///
    /// One failing resource never stops the pass; the failure is
    /// recorded in the report and the pass continues.
    ///
    /// [`TempManager::cleanup_expired`]: crate::TempManager::cleanup_expired
    pub fn cleanup(&self) -> Result<CleanupReport, Error> {
        self.run(CleanupScope::Manager, &HashMap::new())
    }

    /// Scoped pass: sweep and evict within one namespace only.
    pub(crate) fn cleanup_namespace(
        shared: &Arc<NamespaceShared>,
        namespace: &str,
        view: SweepView,
    ) -> Result<CleanupReport, Error> {
        let mut views = HashMap::new();
        views.insert(namespace.to_owned(), view);
        Self::new(Arc::clone(shared)).run(CleanupScope::Namespace(namespace.to_owned()), &views)
    }

    /// Manager-wide pass with per-namespace views resolved by the caller.
    pub(crate) fn cleanup_manager(
        shared: &Arc<NamespaceShared>,
        views: &HashMap<String, SweepView>,
    ) -> Result<CleanupReport, Error> {
        Self::new(Arc::clone(shared)).run(CleanupScope::Manager, views)
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(
        skip(self, scope, views),
        name = "cleanup_pass",
        level = "info",
        fields(scope = scope_str(&scope)),
    ))]
    fn run(
        &self,
        scope: CleanupScope,
        views: &HashMap<String, SweepView>,
    ) -> Result<CleanupReport, Error> {
        let started = Instant::now();
        let now = SystemTime::now();
        let mut report = CleanupReport::default();

        let manager_policy = self
            .shared
            .cleanup_policy
            .read()
            .ok()
            .and_then(|guard| *guard)
            .unwrap_or_default();
        let manager_custom = self
            .shared
            .custom_cleanup_policy
            .read()
            .ok()
            .and_then(|guard| guard.clone());

        let in_scope = |meta: &ResourceMeta| match &scope {
            CleanupScope::Manager => true,
            CleanupScope::Namespace(name) => &meta.namespace == name,
        };

        // -- phase A: expiry, custom policy, pre-marked eligibility ---------
        for meta in snapshot(&self.shared.registry, &scope)? {
            report.scanned += 1;
            if !in_scope(&meta) {
                continue;
            }
            let ns_view = views.get(&meta.namespace);
            let ctx = CleanupContext {
                now,
                scope: scope.clone(),
            };
            // most specific wins: the resource's own TTL, then its
            // namespace's effective policy, then the manager-wide one
            let effective_ttl = meta
                .ttl
                .or_else(|| ns_view.and_then(|v| v.policy.ttl))
                .or(manager_policy.ttl);
            let candidate = classify(
                &meta,
                now,
                effective_ttl,
                ns_view
                    .and_then(|v| v.custom.as_ref())
                    .or(manager_custom.as_ref()),
                &ctx,
            );
            let Some((tag, needs_transition)) = candidate else {
                report.skipped += 1;
                continue;
            };
            if needs_transition {
                match self
                    .shared
                    .registry
                    .set_state(&meta.id, ResourceState::Expired)
                {
                    Ok(_) => {}
                    Err(source) => {
                        report.failed += 1;
                        #[cfg(feature = "tracing")]
                        tracing::error!(id = %meta.id, error = %source, "failed to move the resource to `Expired`");
                        report
                            .failure_reasons
                            .push(format!("{}: {source}", meta.id));
                        bump_attempts(&self.shared.registry, &meta.id);
                        continue;
                    }
                }
            }
            destroy_and_account(&self.shared, &meta, tag, &mut report);
        }

        // -- phase B: per-namespace quota eviction ---------------------------
        // anything the sweep phase reclaimed is gone by now; enforce each
        // namespace's own limits before the manager-wide ones.
        let ns_names: Vec<String> = views.keys().cloned().collect();
        for name in ns_names {
            let Some(view) = views.get(&name) else {
                continue;
            };
            let bytes_limit = view.policy.quota.max_bytes.or(view.quota.max_bytes);
            let count_limit = view.policy.quota.max_resources.or(view.quota.max_resources);
            if bytes_limit.is_none() && count_limit.is_none() {
                continue;
            }
            let metas = live_candidates(&self.shared.registry, &scope, Some(&name))?;
            enforce_limits(&self.shared, metas, bytes_limit, count_limit, &mut report);
        }

        // -- phase C: manager-wide quota eviction -----------------------------
        if matches!(scope, CleanupScope::Manager) {
            let bytes_limit = manager_policy
                .quota
                .max_bytes
                .or(self.shared.config.max_size);
            let count_limit = manager_policy
                .quota
                .max_resources
                .or(self.shared.config.max_resources);
            if bytes_limit.is_some() || count_limit.is_some() {
                let metas = live_candidates(&self.shared.registry, &scope, None)?;
                enforce_limits(&self.shared, metas, bytes_limit, count_limit, &mut report);
            }
        }

        report.duration = started.elapsed();
        let scope_label = scope_str(&scope);
        metrics_support::cleanup_run(scope_label);
        metrics_support::cleanup_duration(scope_label, report.duration);
        #[cfg(feature = "tracing")]
        tracing::info!(
            scope = scope_label,
            scanned = report.scanned,
            removed = report.removed,
            skipped = report.skipped,
            failed = report.failed,
            deferred = report.deferred,
            bytes_freed = report.bytes_freed,
            duration_ms = report.duration.as_millis() as u64,
            "cleanup pass completed"
        );
        self.shared
            .emit(crate::observability::Event::CleanupCompleted {
                instance: self.shared.config.instance_id.clone(),
                scanned: report.scanned,
                removed: report.removed,
                failed: report.failed,
                bytes_freed: report.bytes_freed,
            });
        Ok(report)
    }
}

fn snapshot(registry: &Registry, scope: &CleanupScope) -> Result<Vec<ResourceMeta>, Error> {
    match scope {
        CleanupScope::Manager => Ok(registry.snapshot()?),
        CleanupScope::Namespace(name) => Ok(registry.snapshot_namespace(name)?),
    }
}

/// Resources that may still be evicted: live, not pre-marked for cleanup.
fn live_candidates(
    registry: &Registry,
    scope: &CleanupScope,
    namespace: Option<&str>,
) -> Result<Vec<ResourceMeta>, Error> {
    let mut metas = snapshot(registry, scope)?;
    if let Some(name) = namespace {
        metas.retain(|m| m.namespace == name);
    }
    metas.retain(|m| {
        matches!(
            m.state,
            ResourceState::Created | ResourceState::Active | ResourceState::Locked
        )
    });
    // eviction order: weakest priority first, stalest first within a tier
    metas.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.last_accessed.cmp(&b.last_accessed))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(metas)
}

/// Decide whether `meta` is a cleanup candidate.
///
/// Returns the accounting tag and whether the record still needs its
/// state moved to `Expired` (already done for `Expired`/`PendingCleanup`
/// and `FailedCleanup` records).
fn classify(
    meta: &ResourceMeta,
    now: SystemTime,
    effective_ttl: Option<Ttl>,
    custom: Option<&Arc<dyn CustomCleanupPolicy>>,
    ctx: &CleanupContext,
) -> Option<(Tag, bool)> {
    match meta.state {
        ResourceState::FailedCleanup => {
            if meta.cleanup_attempts >= MAX_CLEANUP_ATTEMPTS {
                return None; // gave up; a human should look at it
            }
            Some((Tag::Eligible, false))
        }
        ResourceState::Expired | ResourceState::PendingCleanup => Some((Tag::Eligible, false)),
        ResourceState::Created | ResourceState::Active | ResourceState::Locked => {
            if let Some(ttl) = effective_ttl {
                if ttl.is_expired(meta.created_at, meta.last_accessed, now) {
                    let tag = match ttl {
                        Ttl::Fixed(_) => Tag::TtlFixed,
                        Ttl::Idle(_) => Tag::TtlIdle,
                    };
                    return Some((tag, true));
                }
            }
            if let Some(policy) = custom {
                if policy.decide(meta, ctx) == PolicyDecision::Remove {
                    return Some((Tag::Policy, true));
                }
            }
            None
        }
        _ => None,
    }
}

/// Destroy resources until the aggregate usage fits the limits.
fn enforce_limits(
    shared: &Arc<NamespaceShared>,
    metas: Vec<ResourceMeta>,
    bytes_limit: Option<u64>,
    count_limit: Option<u64>,
    report: &mut CleanupReport,
) {
    let mut bytes: u64 = metas.iter().map(|m| m.size_bytes).sum();
    let mut count: u64 = metas.len() as u64;
    for victim in metas {
        let bytes_over = bytes_limit.is_some_and(|limit| bytes > limit);
        let count_over = count_limit.is_some_and(|limit| count > limit);
        if !bytes_over && !count_over {
            break;
        }
        let tag = if bytes_over {
            Tag::SizeEviction
        } else {
            Tag::CountEviction
        };
        if victim.state != ResourceState::Expired
            && shared
                .registry
                .set_state(&victim.id, ResourceState::Expired)
                .is_err()
        {
            report.failed += 1;
            #[cfg(feature = "tracing")]
            tracing::error!(id = %victim.id, "failed to move the resource to `Expired` before eviction");
            report.failure_reasons.push(format!(
                "{}: failed to move the resource to `Expired` before eviction",
                victim.id
            ));
            bump_attempts(&shared.registry, &victim.id);
            continue;
        }
        // only a destruction (or another claim winning the race) frees
        // the bytes; a lease-deferred resource keeps occupying its slot
        let before_removed = report.removed;
        let before_skipped = report.skipped;
        destroy_and_account(shared, &victim, tag, report);
        if report.removed > before_removed || report.skipped > before_skipped {
            bytes = bytes.saturating_sub(victim.size_bytes);
            count = count.saturating_sub(1);
        }
    }
}

/// Destroy one resource through the lease-gated path and account for it.
fn destroy_and_account(
    shared: &Arc<NamespaceShared>,
    meta: &ResourceMeta,
    tag: Tag,
    report: &mut CleanupReport,
) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        id = %meta.id,
        namespace = %meta.namespace,
        kind = %meta.kind,
        reason = tag.reason(),
        bytes = meta.size_bytes,
        "evicting resource"
    );
    match destroy_resource(shared, &meta.id, "cleanup") {
        Ok(DestroyOutcome::Destroyed) => {
            report.removed += 1;
            report.bytes_freed = report.bytes_freed.saturating_add(meta.size_bytes);
            tag.charge(report);
        }
        Ok(DestroyOutcome::AlreadyGone) => {
            // someone else won the race; not an error
            report.skipped += 1;
        }
        Ok(DestroyOutcome::Deferred) => {
            report.deferred += 1;
            report.reasons.lease_deferred += 1;
        }
        Err(source) => {
            report.failed += 1;
            report
                .failure_reasons
                .push(format!("{}: {source}", meta.id));
            bump_attempts(&shared.registry, &meta.id);
        }
    }
}

fn bump_attempts(registry: &Registry, id: &crate::core::resource::ResourceId) {
    let _ = registry.update(id, |m| {
        m.cleanup_attempts = m.cleanup_attempts.saturating_add(1);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::{ResourceKind, ResourceMeta};

    #[test]
    fn reclaims_only_eligible_resources() {
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        let mut expired = ResourceMeta::new("gone", "ns", ResourceKind::File);
        expired.size_bytes = 42;
        expired.state = ResourceState::Expired;
        // the physical side was never materialized; backend release must tolerate that
        let active = ResourceMeta::new("live", "ns", ResourceKind::File);
        shared.registry.register(expired).unwrap();
        shared.registry.register(active).unwrap();

        let report = engine.cleanup().unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.removed, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.bytes_freed, 42);
        assert_eq!(report.reasons.eligible, 1);
        assert_eq!(shared.registry.len().unwrap(), 1);
    }

    #[test]
    fn leased_resource_is_deferred_not_destroyed() {
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        let mut pending = ResourceMeta::new("busy", "ns", ResourceKind::File);
        pending.state = ResourceState::PendingCleanup;
        shared.registry.register(pending.clone()).unwrap();

        let lease = shared.leases.acquire(&pending.id).unwrap();
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.deferred, 1);
        assert_eq!(report.reasons.lease_deferred, 1);
        // record survives, marked pending
        let meta = shared.registry.get(&pending.id).unwrap();
        assert_eq!(meta.state, ResourceState::PendingCleanup);

        drop(lease);
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(report.deferred, 0);
        assert!(shared.registry.get(&pending.id).is_err());
    }

    #[test]
    fn reasons_add_up() {
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        for name in ["a", "b"] {
            let mut meta = ResourceMeta::new(name, "ns", ResourceKind::File);
            meta.state = ResourceState::Expired;
            shared.registry.register(meta).unwrap();
        }
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, report.reasons.destroyed());
        assert_eq!(report.deferred, report.reasons.lease_deferred);
        assert_eq!(report.reasons.eligible, 2);
    }

    #[test]
    fn failed_cleanup_is_retried_until_the_attempt_budget_runs_out() {
        // one below the cap: the next pass must try again
        let dir = crate::testsupport::TestDir::new();
        let shared = crate::namespace::test_shared(dir.path());
        let engine = CleanupEngine::new(Arc::clone(&shared));

        let mut retryable = ResourceMeta::new("almost", "ns", ResourceKind::File);
        retryable.state = ResourceState::FailedCleanup;
        retryable.cleanup_attempts = crate::cleanup::MAX_CLEANUP_ATTEMPTS - 1;
        shared.registry.register(retryable).unwrap();
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 1);

        // at the cap: the record is left alone for a human to inspect
        let mut exhausted = ResourceMeta::new("spent", "ns", ResourceKind::File);
        exhausted.state = ResourceState::FailedCleanup;
        exhausted.cleanup_attempts = crate::cleanup::MAX_CLEANUP_ATTEMPTS;
        shared.registry.register(exhausted.clone()).unwrap();
        let report = engine.cleanup().unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 1);
        // the record survives for inspection
        let meta = shared.registry.get(&exhausted.id).unwrap();
        assert_eq!(meta.state, ResourceState::FailedCleanup);
        assert_eq!(meta.cleanup_attempts, crate::cleanup::MAX_CLEANUP_ATTEMPTS);
    }
}
