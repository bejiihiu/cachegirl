//! Roundtrip and compatibility tests for the `serde` feature.
//!
//! The crate ships its data-shaped public types (metadata, policies,
//! reports, statistics, events, requests) as plain serializable values.
//! This file pins down three properties per type:
//!
//! - a value survives serialize -> deserialize unchanged;
//! - timestamps and durations keep their nanosecond precision and their
//!   documented JSON shape;
//! - payloads written without the fields added later still load
//!   (defaults kick in), and unknown fields are ignored rather than
//!   rejected.

#![cfg(feature = "serde")]

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cachegirl::backend::ResourceRequest;
use cachegirl::policy::Ttl;
use cachegirl::recovery::{RecoveryAction, RecoveryEntry, RecoveryPolicy, RecoveryReport};
use cachegirl::{
    CleanupContext, CleanupPolicy, CleanupReport, CleanupScope, CreateOptions, Event,
    ManagerConfig, NamespaceQuota, Ownership, PolicyDecision, Priority, Quota, RemovalReasons,
    ResourceId, ResourceKind, ResourceMeta, ResourceState, Statistics, SymlinkPolicy,
};

fn roundtrip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string_pretty(value).expect("serialize");
    serde_json::from_str(&json).expect("deserialize")
}

fn a_time(secs: u64, nanos: u32) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, nanos)
}

fn a_meta() -> ResourceMeta {
    let mut meta = ResourceMeta::new("packet.bin", "protocol", ResourceKind::File);
    meta.owner = Some("network".to_string());
    meta.created_at = a_time(1_763_100_000, 123_456_700);
    meta.last_accessed = a_time(1_763_100_500, 987_654_300);
    meta.size_bytes = 4096;
    meta.state = ResourceState::Active;
    meta.ttl = Some(Ttl::Idle(Duration::from_secs(900)));
    meta.ownership = Ownership::Shared;
    meta.priority = Priority::High;
    meta.cleanup_attempts = 2;
    meta.metadata
        .insert("request".to_string(), "req-42".to_string());
    meta.metadata
        .insert("backend".to_string(), "filesystem".to_string());
    meta
}

#[test]
fn resource_id_roundtrip() {
    let id = ResourceId::new();
    let back = roundtrip(&id);
    assert_eq!(back, id);
    // transparent shape: a bare string, not an object
    let json = serde_json::to_value(&id).unwrap();
    assert!(json.is_string());
}

#[test]
fn unit_like_enums_roundtrip() {
    for value in [
        Ownership::Owned,
        Ownership::Managed,
        Ownership::Shared,
        Ownership::Leased,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        ResourceKind::File,
        ResourceKind::Directory,
        ResourceKind::Buffer,
        ResourceKind::Cache,
        ResourceKind::Serialization,
        ResourceKind::Runtime,
        ResourceKind::Custom,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        ResourceState::Uninitialized,
        ResourceState::Created,
        ResourceState::Active,
        ResourceState::Locked,
        ResourceState::Expired,
        ResourceState::PendingCleanup,
        ResourceState::Destroyed,
        ResourceState::FailedCleanup,
        ResourceState::Invalid,
        ResourceState::Recovering,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Critical,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        SymlinkPolicy::Reject,
        SymlinkPolicy::WithinRoot,
        SymlinkPolicy::Allow,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [PolicyDecision::Keep, PolicyDecision::Remove] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        RecoveryPolicy::Never,
        RecoveryPolicy::ReportOnly,
        RecoveryPolicy::OnStartup,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
    for value in [
        RecoveryAction::Removed,
        RecoveryAction::Kept,
        RecoveryAction::Skipped,
        RecoveryAction::Failed,
    ] {
        assert_eq!(roundtrip(&value), value);
    }
}

#[test]
fn ttl_serializes_externally_tagged_with_duration_shape() {
    let fixed = Ttl::Fixed(Duration::new(60, 250));
    let json = serde_json::to_value(fixed).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"Fixed": {"secs": 60, "nanos": 250}})
    );
    assert_eq!(roundtrip(&fixed), fixed);

    let idle = Ttl::Idle(Duration::new(7, 0));
    let json = serde_json::to_value(idle).unwrap();
    assert_eq!(json, serde_json::json!({"Idle": {"secs": 7, "nanos": 0}}));
    assert_eq!(roundtrip(&idle), idle);
}

#[test]
fn cleanup_scope_tag_shape() {
    assert_eq!(
        serde_json::to_value(CleanupScope::Manager).unwrap(),
        serde_json::json!("Manager")
    );
    assert_eq!(
        serde_json::to_value(CleanupScope::Namespace("net".to_string())).unwrap(),
        serde_json::json!({"Namespace": "net"})
    );
    assert_eq!(roundtrip(&CleanupScope::Manager), CleanupScope::Manager);
    assert_eq!(
        roundtrip(&CleanupScope::Namespace("net".to_string())),
        CleanupScope::Namespace("net".to_string())
    );
}

#[test]
fn resource_meta_roundtrip_full() {
    let meta = a_meta();
    let back = roundtrip(&meta);
    assert_eq!(back, meta);
}

#[test]
fn resource_meta_roundtrip_minimal() {
    let meta = ResourceMeta::new("minimal", "ns", ResourceKind::Buffer);
    let back = roundtrip(&meta);
    assert_eq!(back, meta);
}

#[test]
fn resource_meta_timestamps_keep_nanos() {
    let meta = a_meta();
    let json = serde_json::to_value(&meta).unwrap();
    assert_eq!(
        json["created_at"],
        serde_json::json!({"secs_since_epoch": 1_763_100_000u64, "nanos_since_epoch": 123_456_700u64})
    );
    assert_eq!(
        json["last_accessed"],
        serde_json::json!({"secs_since_epoch": 1_763_100_500u64, "nanos_since_epoch": 987_654_300u64})
    );
    let back: ResourceMeta = serde_json::from_value(json).unwrap();
    assert_eq!(back.created_at, meta.created_at);
    assert_eq!(back.last_accessed, meta.last_accessed);
}

#[test]
fn resource_meta_written_without_late_fields_still_loads() {
    // shape older writers emit: no ttl/priority/cleanup_attempts/metadata/owner
    let json = serde_json::json!({
        "id": "res-abc-1-42",
        "name": "old.bin",
        "namespace": "legacy",
        "kind": "File",
        "created_at": {"secs_since_epoch": 1_700_000_000u64, "nanos_since_epoch": 0u64},
        "last_accessed": {"secs_since_epoch": 1_700_000_100u64, "nanos_since_epoch": 0u64},
        "size_bytes": 12,
        "state": "Created",
        "ownership": "Managed",
    });
    let meta: ResourceMeta = serde_json::from_value(json).expect("old record must load");
    assert_eq!(meta.ttl, None);
    assert_eq!(meta.priority, Priority::Normal);
    assert_eq!(meta.cleanup_attempts, 0);
    assert_eq!(meta.metadata, HashMap::new());
    assert_eq!(meta.owner, None);
}

#[test]
fn resource_meta_ignores_unknown_fields() {
    let meta = a_meta();
    let mut json = serde_json::to_value(&meta).unwrap();
    json.as_object_mut().unwrap().insert(
        "future_field".to_string(),
        serde_json::json!({"nested": true}),
    );
    let back: ResourceMeta = serde_json::from_value(json).expect("unknown fields must be ignored");
    assert_eq!(back, meta);
}

#[test]
fn quota_and_namespace_quota_roundtrip() {
    for quota in [
        Quota::unlimited(),
        Quota {
            max_bytes: Some(1024),
            max_resources: None,
        },
        Quota {
            max_bytes: None,
            max_resources: Some(7),
        },
        Quota {
            max_bytes: Some(1024),
            max_resources: Some(7),
        },
    ] {
        assert_eq!(roundtrip(&quota), quota);
    }
    for quota in [
        NamespaceQuota::unlimited(),
        NamespaceQuota::with_max_bytes(2048),
        NamespaceQuota::with_max_resources(3),
        NamespaceQuota::with_limits(2048, 3),
    ] {
        assert_eq!(roundtrip(&quota), quota);
    }
}

#[test]
fn cleanup_policy_roundtrip() {
    let policy = CleanupPolicy {
        ttl: Some(Ttl::Fixed(Duration::from_secs(3600))),
        quota: Quota {
            max_bytes: Some(512 * 1024 * 1024),
            max_resources: Some(10_000),
        },
        priority: Priority::High,
    };
    assert_eq!(roundtrip(&policy), policy);

    let default_policy = CleanupPolicy::default();
    assert_eq!(roundtrip(&default_policy), default_policy);

    let idle_policy = CleanupPolicy {
        ttl: Some(Ttl::Idle(Duration::from_secs(30))),
        ..CleanupPolicy::default()
    };
    assert_eq!(roundtrip(&idle_policy), idle_policy);
}

#[test]
fn cleanup_context_roundtrip() {
    let ctx = CleanupContext {
        now: a_time(1_763_100_000, 400),
        scope: CleanupScope::Namespace("protocol".to_string()),
    };
    assert_eq!(roundtrip(&ctx), ctx);

    let manager_scope = CleanupContext {
        now: a_time(1_763_100_000, 0),
        scope: CleanupScope::Manager,
    };
    assert_eq!(roundtrip(&manager_scope), manager_scope);
}

#[test]
fn create_options_roundtrip_and_empty_object_defaults() {
    let mut options = CreateOptions::new()
        .owner("billing")
        .ttl(Duration::from_secs(60))
        .ownership(Ownership::Leased)
        .priority(Priority::Low)
        .metadata("k", "v");
    options.idle_ttl = None;
    assert_eq!(roundtrip(&options), options);

    // an empty object must produce the documented defaults
    let parsed: CreateOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed, CreateOptions::default());
    assert_eq!(parsed.ownership, Ownership::Managed);
    assert!(parsed.metadata.is_empty());

    let with_idle = CreateOptions::new().idle_ttl(Duration::from_secs(5));
    assert_eq!(roundtrip(&with_idle), with_idle);
}

#[test]
fn manager_config_roundtrip() {
    let config = ManagerConfig {
        root: std::path::PathBuf::from("/tmp/cachegirl-test-root"),
        create_if_missing: true,
        cleanup_on_start: true,
        cleanup_on_shutdown: false,
        max_size: Some(1024 * 1024),
        max_resources: Some(500),
        default_ttl: Some(Duration::from_secs(3600)),
        symlink_policy: SymlinkPolicy::WithinRoot,
        instance_id: "instance-test".to_string(),
        recovery_policy: RecoveryPolicy::ReportOnly,
        stale_instance_after: Duration::from_secs(600),
    };
    assert_eq!(roundtrip(&config), config);
}

#[test]
fn manager_config_written_with_only_required_fields_loads_with_defaults() {
    // root and instance_id are required; everything else has a default
    let json = serde_json::json!({
        "root": "/tmp/cachegirl-test-root",
        "instance_id": "instance-test",
    });
    let config: ManagerConfig = serde_json::from_value(json).expect("minimal config must load");
    assert!(config.create_if_missing);
    assert!(!config.cleanup_on_start);
    assert!(!config.cleanup_on_shutdown);
    assert_eq!(config.max_size, None);
    assert_eq!(config.max_resources, None);
    assert_eq!(config.default_ttl, None);
    assert_eq!(config.symlink_policy, SymlinkPolicy::Reject);
    assert_eq!(config.recovery_policy, RecoveryPolicy::OnStartup);
    assert_eq!(config.stale_instance_after, Duration::from_secs(3600));

    // missing root must be an error, not a silent empty path
    let json = serde_json::json!({ "instance_id": "instance-test" });
    assert!(serde_json::from_value::<ManagerConfig>(json).is_err());
}

#[test]
fn removal_reasons_roundtrip_and_totals() {
    let reasons = RemovalReasons {
        ttl_fixed: 2,
        ttl_idle: 1,
        size_eviction: 3,
        count_eviction: 4,
        policy: 5,
        lease_deferred: 6,
        eligible: 7,
    };
    assert_eq!(roundtrip(&reasons), reasons);
    // lease_deferred is not part of destroyed()
    assert_eq!(reasons.destroyed(), 2 + 1 + 3 + 4 + 5 + 7);
    assert!(!reasons.is_quiet());
    assert!(RemovalReasons::default().is_quiet());
}

#[test]
fn cleanup_report_roundtrip_and_display() {
    let report = CleanupReport {
        scanned: 10,
        removed: 7,
        skipped: 2,
        failed: 1,
        deferred: 1,
        bytes_freed: 8192,
        duration: Duration::new(1, 500),
        failure_reasons: vec!["instance-x: io error: disk full".to_string()],
        reasons: RemovalReasons {
            ttl_fixed: 2,
            eligible: 5,
            lease_deferred: 1,
            ..RemovalReasons::default()
        },
    };
    let back = roundtrip(&report);
    assert_eq!(back, report);

    let rendered = format!("{report}");
    assert!(rendered.contains("ttl_fixed=2"), "{rendered}");
    assert!(rendered.contains("eligible=5"), "{rendered}");
    assert!(rendered.contains("lease_deferred=1"), "{rendered}");

    let empty = CleanupReport::default();
    let rendered = format!("{empty}");
    assert!(
        !rendered.contains('('),
        "empty report prints no breakdown: {rendered}"
    );
    assert_eq!(roundtrip(&empty), empty);
}

#[test]
fn cleanup_report_written_without_late_fields_still_loads() {
    // older shape: no deferred, no failure_reasons, no reasons
    let json = serde_json::json!({
        "scanned": 4,
        "removed": 3,
        "skipped": 1,
        "failed": 0,
        "bytes_freed": 100,
        "duration": {"secs": 0, "nanos": 123456},
    });
    let report: CleanupReport = serde_json::from_value(json).expect("old report must load");
    assert_eq!(report.deferred, 0);
    assert!(report.failure_reasons.is_empty());
    assert_eq!(report.reasons, RemovalReasons::default());
    assert_eq!(report.duration, Duration::new(0, 123_456));
}

#[test]
fn recovery_report_roundtrip_and_display() {
    let report = RecoveryReport {
        scanned: 3,
        removed: 1,
        kept: 1,
        skipped: 1,
        failed: 1,
        bytes_freed: 4096,
        entries: vec![
            RecoveryEntry {
                instance: "instance-dead".to_string(),
                action: RecoveryAction::Removed,
                reason: "pid 2147483646 is not running".to_string(),
                bytes_freed: 4096,
            },
            RecoveryEntry {
                instance: "instance-live".to_string(),
                action: RecoveryAction::Kept,
                reason: "pid 4242 is running".to_string(),
                bytes_freed: 0,
            },
        ],
        failure_reasons: vec!["instance-broken: removal failed: permission denied".to_string()],
    };
    let back = roundtrip(&report);
    assert_eq!(back, report);

    let rendered = format!("{report}");
    assert!(rendered.contains("removed 1"), "{rendered}");
    assert!(rendered.contains("failure: instance-broken"), "{rendered}");
}

#[test]
fn recovery_report_written_without_late_fields_still_loads() {
    // older shape: no skipped, bytes_freed, entries or failure_reasons
    let json = serde_json::json!({
        "scanned": 2,
        "removed": 1,
        "kept": 1,
        "failed": 0,
    });
    let report: RecoveryReport = serde_json::from_value(json).expect("old report must load");
    assert_eq!(report.skipped, 0);
    assert_eq!(report.bytes_freed, 0);
    assert!(report.entries.is_empty());
    assert!(report.failure_reasons.is_empty());
}

#[test]
fn statistics_roundtrip_and_timestamp_shape() {
    let snapshot = Statistics {
        instance_id: "instance-test".to_string(),
        captured_at: Some(a_time(1_763_100_000, 700)),
        namespaces: 2,
        resources: 5,
        bytes_tracked: 1024,
        expired: 1,
        pending_cleanup: 1,
        gc_runs: 3,
        resources_released: 9,
        bytes_freed: 2048,
        active_leases: 4,
    };
    let back = roundtrip(&snapshot);
    assert_eq!(back, snapshot);

    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        json["captured_at"],
        serde_json::json!({"secs_since_epoch": 1_763_100_000u64, "nanos_since_epoch": 700u64})
    );

    let unstamped = Statistics::default();
    let json = serde_json::to_value(&unstamped).unwrap();
    assert_eq!(json["captured_at"], serde_json::json!(null));
    assert_eq!(roundtrip(&unstamped), unstamped);

    let rendered = unstamped.render();
    assert!(rendered.contains("released:"), "{rendered}");
}

#[test]
fn events_roundtrip_each_variant() {
    let id = ResourceId::new();
    let events = vec![
        Event::ResourceCreated {
            instance: "instance-test".to_string(),
            id: id.clone(),
            kind: ResourceKind::Directory,
            namespace: "protocol".to_string(),
        },
        Event::ResourceReleased {
            instance: "instance-test".to_string(),
            id: id.clone(),
            bytes_freed: 4096,
        },
        Event::CleanupCompleted {
            instance: "instance-test".to_string(),
            scanned: 10,
            removed: 4,
            failed: 1,
            bytes_freed: 2048,
        },
        Event::RecoveryCompleted {
            instance: "instance-test".to_string(),
            scanned: 3,
            removed: 1,
            kept: 1,
            failed: 1,
            bytes_freed: 512,
        },
    ];
    for event in &events {
        let back = roundtrip(event);
        assert_eq!(&back, event);
    }

    // externally tagged: the variant name is the top-level key
    let json = serde_json::to_value(&events[0]).unwrap();
    assert!(json.get("ResourceCreated").is_some(), "{json}");
    assert_eq!(
        json["ResourceCreated"]["kind"],
        serde_json::json!("Directory")
    );

    let json = serde_json::to_value(&events[2]).unwrap();
    assert_eq!(json["CleanupCompleted"]["removed"], serde_json::json!(4));
}

#[test]
fn resource_request_roundtrip() {
    let request =
        ResourceRequest::new("packet.bin", "protocol", ResourceKind::File).with_size_hint(4096);
    let mut request = request;
    request
        .metadata
        .insert("backend".to_string(), "filesystem".to_string());
    assert_eq!(roundtrip(&request), request);

    let bare = ResourceRequest::new("x", "y", ResourceKind::Runtime);
    assert_eq!(roundtrip(&bare), bare);

    // size_hint/metadata may be omitted entirely
    let json = serde_json::json!({
        "name": "x",
        "namespace": "y",
        "kind": "Runtime",
    });
    let parsed: ResourceRequest = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, bare);
}

#[test]
fn struct_payloads_ignore_unknown_fields() {
    let mut json = serde_json::to_value(a_meta()).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("surprise".to_string(), serde_json::json!("boo"));
    let json2 = serde_json::to_value(CleanupReport::default())
        .map(|mut v| {
            v.as_object_mut()
                .unwrap()
                .insert("surprise".to_string(), serde_json::json!(1));
            v
        })
        .unwrap();
    let _: ResourceMeta = serde_json::from_value(json).unwrap();
    let back: CleanupReport = serde_json::from_value(json2).unwrap();
    assert_eq!(back, CleanupReport::default());
}

#[cfg(feature = "memory")]
#[test]
fn buffer_roundtrip_transparent() {
    let buffer = cachegirl::resource::Buffer::from_vec(vec![1, 2, 3, 250]);
    let json = serde_json::to_value(&buffer).unwrap();
    assert_eq!(json, serde_json::json!([1, 2, 3, 250]));
    assert_eq!(roundtrip(&buffer), buffer);
    assert_eq!(
        roundtrip(&cachegirl::resource::Buffer::default()),
        cachegirl::resource::Buffer::default()
    );
}

#[cfg(feature = "cache")]
#[test]
fn cache_entry_roundtrip() {
    let entry = cachegirl::resource::CacheEntry {
        key: "pkt-1".to_string(),
        value: vec![9, 8, 7],
        created_at: a_time(1_763_100_000, 500),
        last_accessed: a_time(1_763_100_100, 600),
    };
    let back = roundtrip(&entry);
    assert_eq!(back, entry);

    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        json["created_at"],
        serde_json::json!({"secs_since_epoch": 1_763_100_000u64, "nanos_since_epoch": 500u64})
    );

    let loaded: cachegirl::resource::CacheEntry = serde_json::from_value(json).unwrap();
    assert_eq!(loaded, entry);
}
