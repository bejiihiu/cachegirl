//! Behavioral tests for TTL expiry, quota eviction, custom policies
//! and the cleanup report breakdown.
//!
//! Timestamps are backdated through the public registry API instead of
//! sleeping, so the suite stays deterministic.

#![cfg(any(feature = "memory", feature = "filesystem"))]

#[cfg(feature = "memory")]
use cachegirl::ResourceId;
#[cfg(feature = "memory")]
use cachegirl::ResourceKind;
#[cfg(feature = "memory")]
use cachegirl::{
    CleanupContext, CleanupPolicy, Error, PolicyDecision, ResourceMeta, ResourceState,
};
use cachegirl::{CreateOptions, Priority};
#[cfg(feature = "memory")]
use std::sync::Arc;
#[cfg(feature = "memory")]
use std::time::{Duration, SystemTime};

#[cfg(feature = "memory")]
fn backdate(manager: &cachegirl::TempManager, id: &ResourceId, seconds: u64) {
    let past = SystemTime::now() - Duration::from_secs(seconds);
    manager
        .registry()
        .update(id, |m| {
            m.created_at = past;
            m.last_accessed = past;
        })
        .unwrap();
}

#[cfg(feature = "memory")]
fn backdate_access(manager: &cachegirl::TempManager, id: &ResourceId, seconds: u64) {
    let past = SystemTime::now() - Duration::from_secs(seconds);
    manager
        .registry()
        .update(id, |m| m.last_accessed = past)
        .unwrap();
}

#[cfg(feature = "memory")]
fn buffer(ns: &cachegirl::Namespace, size: usize) -> cachegirl::BufferResource {
    ns.create_buffer(size).unwrap()
}

fn test_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cachegirl-cleanup-{tag}-{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(feature = "memory")]
#[test]
fn fixed_ttl_expires_from_creation_even_with_recent_access() {
    let dir = test_dir("fixed-ttl");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-fixed")
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();

    let buf = ns
        .create_buffer_with(32, CreateOptions::new().ttl(Duration::from_secs(3600)))
        .unwrap();
    let id = buf.id().clone();
    manager
        .registry()
        .update(&id, |m| {
            m.created_at = SystemTime::now() - Duration::from_secs(7200)
        })
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.ttl_fixed, 1);
    assert_eq!(report.reasons.ttl_idle, 0);
    assert!(manager.registry().get(&id).is_err());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn idle_ttl_expires_from_last_access() {
    let dir = test_dir("idle-ttl");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-idle")
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();

    let stale = ns
        .create_buffer_with(32, CreateOptions::new().idle_ttl(Duration::from_secs(600)))
        .unwrap();
    let fresh = ns
        .create_buffer_with(32, CreateOptions::new().idle_ttl(Duration::from_secs(600)))
        .unwrap();
    // creation time backdated for both; only one is idle past the ttl
    backdate(&manager, stale.id(), 10_000);
    manager
        .registry()
        .update(fresh.id(), |m| {
            m.created_at = SystemTime::now() - Duration::from_secs(10_000)
        })
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.ttl_idle, 1);
    assert!(manager.registry().get(stale.id()).is_err());
    assert!(manager.registry().get(fresh.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn recently_touched_idle_resource_survives() {
    let dir = test_dir("idle-fresh");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-fresh")
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();

    let buf = ns
        .create_buffer_with(16, CreateOptions::new().idle_ttl(Duration::from_secs(60)))
        .unwrap();
    let id = buf.id().clone();
    // ancient creation, access a second ago: idle ttl must not fire
    backdate(&manager, &id, 10_000);
    manager
        .registry()
        .update(&id, |m| m.last_accessed = SystemTime::now())
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.scanned, 1);
    assert!(manager.registry().get(&id).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn expired_leased_resource_is_deferred_not_destroyed() {
    let dir = test_dir("defer-lease");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-defer")
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();

    let buf = ns
        .create_buffer_with(16, CreateOptions::new().ttl(Duration::from_secs(3600)))
        .unwrap();
    let lease = buf.acquire().unwrap();
    backdate(&manager, buf.id(), 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.reasons.lease_deferred, 1);
    let meta = manager.registry().get(buf.id()).unwrap();
    assert_eq!(meta.state, ResourceState::PendingCleanup);

    drop(lease);
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.eligible, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn manager_default_ttl_is_enforced_by_cleanup() {
    let dir = test_dir("default-ttl");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-default")
        .default_ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();
    let buf = buffer(&ns, 8);
    backdate(&manager, buf.id(), 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.ttl_fixed, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn manager_policy_ttl_applies_to_untagged_resources() {
    let dir = test_dir("policy-ttl");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-polttl")
        .cleanup_policy(
            CleanupPolicy::builder()
                .ttl(Duration::from_secs(3600))
                .build(),
        )
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();
    let buf = buffer(&ns, 8);
    backdate(&manager, buf.id(), 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.ttl_fixed, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn resource_ttl_beats_policy_ttl() {
    let dir = test_dir("precedence");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-prec")
        .cleanup_policy(CleanupPolicy::builder().ttl(Duration::from_secs(1)).build())
        .build()
        .unwrap();
    let ns = manager.namespace("ttl").unwrap();
    let buf = ns
        .create_buffer_with(
            8,
            CreateOptions::new().idle_ttl(Duration::from_secs(10_000)),
        )
        .unwrap();
    let id = buf.id().clone();
    // both backdated; only the policy's 1s fixed ttl would fire, so a
    // removal would prove the policy beat the resource's own rule
    backdate(&manager, &id, 5000);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert!(manager.registry().get(&id).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn namespace_policy_ttl_scopes_expiry() {
    let dir = test_dir("ns-policy");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-nspol")
        .build()
        .unwrap();
    let limited = manager.namespace("limited").unwrap();
    let plain = manager.namespace("plain").unwrap();
    limited.set_cleanup_policy(
        CleanupPolicy::builder()
            .ttl(Duration::from_secs(3600))
            .build(),
    );

    let a = buffer(&limited, 8);
    let b = buffer(&plain, 8);
    backdate(&manager, a.id(), 7200);
    backdate(&manager, b.id(), 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(manager.registry().get(a.id()).is_err());
    assert!(manager.registry().get(b.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn namespace_scoped_cleanup_ignores_other_namespaces() {
    let dir = test_dir("ns-scoped");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-nsscope")
        .build()
        .unwrap();
    let a = manager.namespace("a").unwrap();
    let b = manager.namespace("b").unwrap();
    a.set_cleanup_policy(
        CleanupPolicy::builder()
            .ttl(Duration::from_secs(60))
            .build(),
    );
    b.set_cleanup_policy(
        CleanupPolicy::builder()
            .ttl(Duration::from_secs(60))
            .build(),
    );

    let ra = buffer(&a, 8);
    let rb = buffer(&b, 8);
    backdate(&manager, ra.id(), 7200);
    backdate(&manager, rb.id(), 7200);

    let report = a.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(manager.registry().get(ra.id()).is_err());
    assert!(manager.registry().get(rb.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn size_eviction_drops_lowest_priority_first() {
    let dir = test_dir("evict-priority");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-evict")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let low = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let normal = buffer(&ns, 64);
    let critical = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Critical))
        .unwrap();
    // over quota now: 192 bytes, limit 100 -> must evict Low and Normal
    ns.set_quota(cachegirl::NamespaceQuota::with_max_bytes(100));

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.reasons.size_eviction, 2);
    assert_eq!(report.removed, 2);
    assert!(manager.registry().get(low.id()).is_err());
    assert!(manager.registry().get(normal.id()).is_err());
    assert!(manager.registry().get(critical.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn size_eviction_stops_once_quota_is_satisfied() {
    let dir = test_dir("evict-bytes");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-bytes")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let low = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let normal = buffer(&ns, 64);
    let high = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::High))
        .unwrap();
    ns.set_quota(cachegirl::NamespaceQuota::with_max_bytes(140));

    let report = manager.cleanup_expired().unwrap();
    // 192 -> 128 after evicting only the low-priority buffer
    assert_eq!(report.reasons.size_eviction, 1);
    assert!(manager.registry().get(low.id()).is_err());
    assert!(manager.registry().get(normal.id()).is_ok());
    assert!(manager.registry().get(high.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn count_eviction_evicts_stalest_within_same_priority() {
    let dir = test_dir("evict-count");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-count")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let old = buffer(&ns, 8);
    let middle = buffer(&ns, 8);
    let fresh = buffer(&ns, 8);
    backdate_access(&manager, old.id(), 7200);
    backdate_access(&manager, middle.id(), 3600);

    ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(1));
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.reasons.count_eviction, 2);
    assert_eq!(report.removed, 2);
    assert!(manager.registry().get(old.id()).is_err());
    assert!(manager.registry().get(middle.id()).is_err());
    assert!(manager.registry().get(fresh.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn manager_wide_count_limit_evicts_across_namespaces() {
    let dir = test_dir("evict-manager");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-mgrcount")
        .cleanup_policy(CleanupPolicy::builder().max_resources(1).build())
        .build()
        .unwrap();
    let one = manager.namespace("one").unwrap();
    let two = manager.namespace("two").unwrap();

    let low = one
        .create_buffer_with(8, CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let critical = two
        .create_buffer_with(8, CreateOptions::new().priority(Priority::Critical))
        .unwrap();
    let normal = two.create_buffer_with(8, CreateOptions::new()).unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.reasons.count_eviction, 2);
    assert!(manager.registry().get(low.id()).is_err());
    assert!(manager.registry().get(normal.id()).is_err());
    assert!(manager.registry().get(critical.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[test]
fn manager_wide_size_limit_evicts_by_priority() {
    let dir = test_dir("evict-mgr-size");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-mgrsize")
        .max_size(100)
        .build()
        .unwrap();
    let ns = manager.namespace("files").unwrap();

    // files are created empty (no size hint), so quota does not block
    // creation; write bytes afterwards and refresh the tracked sizes
    let low = ns
        .create_file_with("low.bin", CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let normal = ns.create_file("normal.bin").unwrap();
    for id in [low.id(), normal.id()] {
        let path = if id == low.id() {
            low.path().to_path_buf()
        } else {
            normal.path().to_path_buf()
        };
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        ns.refresh_size(id).unwrap();
    }

    let report = manager.cleanup_expired().unwrap();
    // 128 bytes tracked, limit 100: the low-priority file must go
    assert_eq!(report.reasons.size_eviction, 1);
    assert!(manager.registry().get(low.id()).is_err());
    assert!(manager.registry().get(normal.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn namespace_quota_enforced_in_manager_pass() {
    let dir = test_dir("ns-quota");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-nsquota")
        .build()
        .unwrap();
    let limited = manager.namespace("limited").unwrap();
    let untouched = manager.namespace("untouched").unwrap();

    let low = limited
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let high = limited
        .create_buffer_with(64, CreateOptions::new().priority(Priority::High))
        .unwrap();
    let other = buffer(&untouched, 64);
    // 128 bytes in "limited" against a 70 byte ns quota
    limited.set_quota(cachegirl::NamespaceQuota::with_max_bytes(70));

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.reasons.size_eviction, 1);
    assert!(manager.registry().get(low.id()).is_err());
    assert!(manager.registry().get(high.id()).is_ok());
    assert!(manager.registry().get(other.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn custom_policy_removes_matching_resources() {
    let dir = test_dir("custom-remove");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-custom")
        .custom_cleanup_policy(Arc::new(|meta: &ResourceMeta, _ctx: &CleanupContext| {
            if meta.owner.as_deref() == Some("junk") {
                PolicyDecision::Remove
            } else {
                PolicyDecision::Keep
            }
        }))
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();
    ns.create_buffer_with(8, CreateOptions::new().owner("junk"))
        .unwrap();
    ns.create_buffer_with(8, CreateOptions::new().owner("keep"))
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.reasons.policy, 1);
    let remaining: Vec<_> = manager
        .registry()
        .snapshot()
        .unwrap()
        .into_iter()
        .map(|m| m.owner.unwrap_or_default())
        .collect();
    assert_eq!(remaining, vec!["keep".to_string()]);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn namespace_custom_policy_overrides_manager_custom_policy() {
    let dir = test_dir("custom-override");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-override")
        .custom_cleanup_policy(Arc::new(|_meta: &ResourceMeta, _ctx: &CleanupContext| {
            PolicyDecision::Remove
        }))
        .build()
        .unwrap();
    let mine = manager.namespace("mine").unwrap();
    mine.set_custom_cleanup_policy(Arc::new(|_meta: &ResourceMeta, _ctx: &CleanupContext| {
        PolicyDecision::Keep
    }));
    let other_ns = manager.namespace("other").unwrap();

    let mine_buf = buffer(&mine, 8);
    let other_buf = buffer(&other_ns, 8);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(manager.registry().get(mine_buf.id()).is_ok());
    assert!(manager.registry().get(other_buf.id()).is_err());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn custom_policy_removal_is_deferred_for_leased_resources() {
    let dir = test_dir("custom-defer");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-cstdefer")
        .custom_cleanup_policy(Arc::new(|_meta: &ResourceMeta, _ctx: &CleanupContext| {
            PolicyDecision::Remove
        }))
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();
    let buf = buffer(&ns, 8);
    let lease = buf.acquire().unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.reasons.lease_deferred, 1);
    assert_eq!(
        manager.registry().get(buf.id()).unwrap().state,
        ResourceState::PendingCleanup
    );
    drop(lease);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn failed_resource_does_not_stop_the_pass() {
    use cachegirl::BackendError;

    struct AlwaysFailBackend;

    impl cachegirl::backend::ResourceBackend for AlwaysFailBackend {
        fn name(&self) -> &str {
            "always-fail"
        }

        fn create(
            &self,
            request: &cachegirl::backend::ResourceRequest,
        ) -> cachegirl::Result<ResourceMeta> {
            Ok(ResourceMeta::new(
                &request.name,
                &request.namespace,
                request.kind,
            ))
        }

        fn release(&self, _meta: &ResourceMeta) -> cachegirl::Result<()> {
            Err(BackendError::Rejected {
                backend: "always-fail".into(),
                reason: "simulated transient io failure".into(),
            }
            .into())
        }
    }

    let dir = test_dir("partial-fail");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-partfail")
        .build()
        .unwrap();
    manager
        .register_backend(Arc::new(AlwaysFailBackend))
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let failing = ns.create_custom("doomed", "always-fail").unwrap();
    let ok = buffer(&ns, 8);
    backdate(&manager, failing.id(), 7200);
    backdate(&manager, ok.id(), 7200);

    // the failing resource's TTL has not expired: force eligibility
    manager
        .registry()
        .set_state(failing.id(), ResourceState::Expired)
        .unwrap();
    manager
        .registry()
        .set_state(ok.id(), ResourceState::Expired)
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.removed, 1);
    assert!(
        report
            .failure_reasons
            .iter()
            .any(|reason| reason.contains(failing.id().as_str()))
    );
    assert!(manager.registry().get(ok.id()).is_err());
    let meta = manager.registry().get(failing.id()).unwrap();
    assert_eq!(meta.state, ResourceState::FailedCleanup);
    assert_eq!(meta.cleanup_attempts, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn failed_cleanup_retries_then_gives_up() {
    use cachegirl::BackendError;

    struct AlwaysFailBackend;

    impl cachegirl::backend::ResourceBackend for AlwaysFailBackend {
        fn name(&self) -> &str {
            "always-fail-2"
        }

        fn create(
            &self,
            request: &cachegirl::backend::ResourceRequest,
        ) -> cachegirl::Result<ResourceMeta> {
            Ok(ResourceMeta::new(
                &request.name,
                &request.namespace,
                request.kind,
            ))
        }

        fn release(&self, _meta: &ResourceMeta) -> cachegirl::Result<()> {
            Err(BackendError::Io {
                backend: "always-fail-2".into(),
                source: std::io::Error::other("disk hiccup"),
            }
            .into())
        }
    }

    let dir = test_dir("retry-limit");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-retry")
        .build()
        .unwrap();
    manager
        .register_backend(Arc::new(AlwaysFailBackend))
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let failing = ns.create_custom("doomed", "always-fail-2").unwrap();
    backdate(&manager, failing.id(), 7200);
    manager
        .registry()
        .set_state(failing.id(), ResourceState::Expired)
        .unwrap();

    // three attempts burn the budget...
    for attempt in 1..=3u32 {
        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.failed, 1, "attempt {attempt}");
        let meta = manager.registry().get(failing.id()).unwrap();
        assert_eq!(meta.cleanup_attempts, attempt);
    }
    // ...then the resource is left in peace, not retried forever
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.failed, 0);
    assert_eq!(report.skipped, 1);
    assert!(manager.registry().get(failing.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn eviction_defers_leased_victim_and_spares_others() {
    let dir = test_dir("evict-defer");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-evdefer")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let held = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Low))
        .unwrap();
    let spare = ns
        .create_buffer_with(64, CreateOptions::new().priority(Priority::Normal))
        .unwrap();
    let lease = held.acquire().unwrap();
    ns.set_quota(cachegirl::NamespaceQuota::with_max_bytes(100));

    let report = manager.cleanup_expired().unwrap();
    // the leased low-priority victim cannot be destroyed; the pass
    // moves on to the rest instead of getting stuck
    assert_eq!(report.deferred, 1);
    assert_eq!(report.reasons.lease_deferred, 1);
    assert_eq!(report.reasons.size_eviction, 1);
    assert!(manager.registry().get(held.id()).is_ok());
    assert!(manager.registry().get(spare.id()).is_err());

    drop(lease);
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(manager.registry().get(held.id()).is_err());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn eviction_reason_splits_size_and_count() {
    let dir = test_dir("evict-split");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-split")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    buffer(&ns, 8);
    buffer(&ns, 8);
    buffer(&ns, 8);
    ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(1));

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.reasons.count_eviction, 2);
    assert_eq!(report.reasons.size_eviction, 0);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn ttl_and_idle_ttl_are_mutually_exclusive() {
    let dir = test_dir("exclusive-ttl");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-excl")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let err = ns
        .create_buffer_with(
            8,
            CreateOptions::new()
                .ttl(Duration::from_secs(10))
                .idle_ttl(Duration::from_secs(10)),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Config(cachegirl::ConfigError::Invalid(_))
    ));
    assert_eq!(manager.registry().len().unwrap(), 0);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn priority_inherits_from_policy_and_create_option_wins() {
    let dir = test_dir("priority-inherit");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-prio")
        .cleanup_policy(
            CleanupPolicy::builder()
                .priority(Priority::Critical)
                .build(),
        )
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let inherited = buffer(&ns, 8);
    assert_eq!(
        manager.registry().get(inherited.id()).unwrap().priority,
        Priority::Critical
    );

    let ns_low = manager.namespace("lowprio").unwrap();
    ns_low.set_cleanup_policy(CleanupPolicy::builder().priority(Priority::Low).build());
    let ns_inherited = buffer(&ns_low, 8);
    assert_eq!(
        manager.registry().get(ns_inherited.id()).unwrap().priority,
        Priority::Low
    );

    let explicit = ns
        .create_buffer_with(8, CreateOptions::new().priority(Priority::High))
        .unwrap();
    assert_eq!(
        manager.registry().get(explicit.id()).unwrap().priority,
        Priority::High
    );
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn clearing_manager_policy_stops_expiry() {
    let dir = test_dir("clear-policy");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-clear")
        .cleanup_policy(
            CleanupPolicy::builder()
                .ttl(Duration::from_secs(60))
                .build(),
        )
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();
    let buf = buffer(&ns, 8);
    backdate_access(&manager, buf.id(), 7200);
    backdate(&manager, buf.id(), 7200);

    manager.clear_cleanup_policy();
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    manager.set_cleanup_policy(
        CleanupPolicy::builder()
            .ttl(Duration::from_secs(60))
            .build(),
    );
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn report_display_and_quiet_helpers() {
    let dir = test_dir("report-display");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-report")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert!(report.is_quiet());
    assert_eq!(report.reasons.destroyed(), 0);
    let line = format!("{report}");
    assert!(line.contains("scanned=0"));

    let buf = ns
        .create_buffer_with(8, CreateOptions::new().ttl(Duration::from_secs(60)))
        .unwrap();
    backdate(&manager, buf.id(), 7200);
    let report = manager.cleanup_expired().unwrap();
    assert!(!report.is_quiet());
    assert_eq!(report.reasons.destroyed(), report.removed);
    assert_eq!(report.reasons.lease_deferred, report.deferred);
    let line = format!("{report}");
    assert!(line.contains("ttl_fixed=1"));
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn bytes_freed_matches_destroyed_sizes() {
    let dir = test_dir("bytes-freed");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-bytes2")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();

    let a = ns
        .create_buffer_with(100, CreateOptions::new().ttl(Duration::from_secs(60)))
        .unwrap();
    let b = ns
        .create_buffer_with(250, CreateOptions::new().ttl(Duration::from_secs(60)))
        .unwrap();
    backdate(&manager, a.id(), 7200);
    backdate(&manager, b.id(), 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 2);
    assert_eq!(report.bytes_freed, 350);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn no_policy_no_expiry() {
    let dir = test_dir("no-policy");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-none")
        .build()
        .unwrap();
    let ns = manager.namespace("mem").unwrap();
    let buf = buffer(&ns, 8);
    backdate(&manager, buf.id(), 31_536_000); // ten years

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert!(report.is_quiet());
    assert!(manager.registry().get(buf.id()).is_ok());
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[test]
fn cleanup_of_uninitialized_and_invalid_states_is_skipped() {
    let dir = test_dir("skip-states");
    let manager = cachegirl::TempManager::builder()
        .root(&dir)
        .instance_id("instance-skip")
        .build()
        .unwrap();

    // plant records in states the sweep must not touch
    let mut invalid = ResourceMeta::new("invalid", "mem", ResourceKind::Runtime);
    invalid.state = ResourceState::Invalid;
    manager.registry().register(invalid).unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.skipped, 1);
    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
