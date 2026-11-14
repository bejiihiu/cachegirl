//! Property-based tests for the invariants every input must satisfy.
//!
//! The hand-written suites pin specific cases; this one fuzzes the rules
//! themselves: names, paths, quotas, TTLs, the lifecycle machine and the
//! registry under arbitrary input. Case counts are fixed so runs stay
//! quiet and reproducible; shrinking is left on for deep dives.
//!
//! The validators and the lexical path resolver are not part of the
//! public API, so where those paths need exercising they are driven
//! through the public backend / manager entry points instead; the tiny
//! reference model used as an oracle is only a checker, never the thing
//! under test.

mod common;

use core::time::Duration;
use std::sync::Arc;
use std::time::SystemTime;

use cachegirl::{
    CleanupPolicy, ConfigError, Error, Priority, Quota, Registry, ResourceKind, ResourceMeta,
    ResourceState, TempManager, Ttl,
};
// часть имён используется только в filesystem-тестах
#[cfg(feature = "filesystem")]
use cachegirl::backend::ResourceBackend;
#[cfg(feature = "filesystem")]
use cachegirl::{AccessError, BackendError};
use common::TestDir;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

// ---------------------------------------------------------------------
// reference oracles (checkers, not the code under test)
// ---------------------------------------------------------------------

// rough mirror of the reserved-device rule, needed by every name oracle
fn is_reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("");
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    RESERVED.contains(&stem.to_ascii_uppercase().as_str())
}

#[cfg(feature = "filesystem")]
fn expect_resource_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed != name || name == "." || name == ".." {
        return false;
    }
    if name.contains(['/', '\\', ':']) {
        return false;
    }
    if name.chars().any(|c| c.is_control()) {
        return false;
    }
    if name.ends_with('.') {
        return false;
    }
    !is_reserved_device(name)
}

fn expect_namespace_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() || trimmed != segment || segment == "." || segment == ".." {
        return false;
    }
    if segment.contains(['/', '\\']) {
        return false;
    }
    if segment.ends_with('.') {
        return false;
    }
    if is_reserved_device(segment) {
        return false;
    }
    segment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn expect_namespace_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > cachegirl::namespace::MAX_NAMESPACE_DEPTH {
        return false;
    }
    segments
        .iter()
        .all(|segment| expect_namespace_segment(segment))
}

fn expect_instance_id(id: &str) -> bool {
    if id.trim().is_empty() || id == "." || id == ".." || id.ends_with('.') {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

// lexical resolution with pop-on-"..", the smallest possible reference;
// `Err(())` means `..` tried to climb above the anchor
#[cfg(feature = "filesystem")]
fn model_normalize(path: &std::path::Path) -> Result<std::path::PathBuf, ()> {
    use std::path::{Component, PathBuf};

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(());
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    Ok(out)
}

#[cfg(feature = "filesystem")]
fn segment_strategy() -> impl Strategy<Value = String> {
    // anything without separators or drive markers; the dot variants travel
    // as their own strategy arms, not as plain segments. Char-vector based
    // because the regex string strategies need the std feature (which pulls
    // getrandom, unbuildable on windows-gnu)
    let segment_char = any::<char>().prop_filter("not a separator or colon", |c| {
        !matches!(c, '/' | '\\' | ':')
    });
    vec(segment_char, 1..=12).prop_filter_map("dot segments are their own strategy arms", |v| {
        let s: String = v.into_iter().collect();
        (s != "." && s != "..").then_some(s)
    })
}

// arbitrary unicode strings, same reason as above
fn unicode_string(max: usize) -> impl Strategy<Value = String> {
    vec(any::<char>(), 0..=max).prop_map(|v| v.into_iter().collect())
}

// ---------------------------------------------------------------------
// signature: release() with an arbitrary crafted path never deletes
// anything outside the root, and the release outcome matches the lexical
// oracle
// ---------------------------------------------------------------------

#[cfg(feature = "filesystem")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn release_keeps_arbitrary_paths_inside_the_root(
        segs in vec(
            prop_oneof![
                2 => segment_strategy(),
                1 => Just(".".to_string()),
                1 => Just("..".to_string()),
            ],
            0..6,
        ),
        payload in vec(any::<u8>(), 1..32),
    ) {
        use std::path::Component;

        let base = TestDir::new("prop-norm");
        let root = std::fs::canonicalize(base.path()).unwrap();
        // something precious, outside the root, that must survive no matter what
        let canary_dir = TestDir::new("prop-norm-canary");
        let canary = canary_dir.path().join("canary.bin");
        std::fs::write(&canary, b"canary-payload").unwrap();

        let backend = cachegirl::backend::FilesystemBackend::new(
            &root,
            "instance-prop",
            cachegirl::SymlinkPolicy::Reject,
        )
        .unwrap();

        let mut crafted = root.join("instance-prop").join("ns");
        for seg in &segs {
            crafted.push(seg);
        }

        let model = model_normalize(&crafted);
        if let Ok(norm) = &model {
            // the oracle itself must be clean: no `.` / `..` in the middle
            prop_assert!(
                norm.components()
                    .all(|c| !matches!(c, Component::CurDir | Component::ParentDir))
            );
        }
        // writing the probe file is only legal inside the root
        let inside_root = model.as_ref().is_ok_and(|norm| norm.starts_with(&root));

        let mut meta = ResourceMeta::new("probe", "ns", ResourceKind::File);
        meta.metadata
            .insert("path".to_string(), crafted.to_string_lossy().into_owned());

        // best-effort: plant the target file where the oracle says it lives.
        // a name like CON still isn't creatable on Windows — then the
        // deletion assert is skipped, everything else still applies
        let materialized = if inside_root {
            model.as_ref().ok().and_then(|norm| {
                if *norm == root {
                    return None;
                }
                let parent = norm.parent()?.to_path_buf();
                std::fs::create_dir_all(&parent).ok()?;
                std::fs::write(norm, &payload).ok()?;
                Some(norm.clone())
            })
        } else {
            None
        };

        let result = backend.release(&meta);

        // the root and the canary survive, whatever else happened
        prop_assert!(base.path().exists());
        prop_assert_eq!(std::fs::read(&canary).unwrap(), b"canary-payload");

        match model {
            Err(()) => {
                // `..` above the base: must be refused as a path escape
                prop_assert!(
                    matches!(result, Err(Error::Backend(BackendError::PathEscape(_)))),
                    "expected PathEscape, got {result:?}"
                );
            }
            Ok(norm) if !norm.starts_with(&root) => {
                // stays above the base but out of the root: refused too
                prop_assert!(
                    matches!(result, Err(Error::Access(AccessError::OutsideRoot { .. }))),
                    "expected OutsideRoot, got {result:?}"
                );
            }
            Ok(norm) if norm == root => {
                // the root's own parent is not under the root; refuse
                prop_assert!(result.is_err());
                prop_assert!(base.path().exists());
            }
            Ok(_) => {
                prop_assert!(result.is_ok(), "release failed: {:?}", result.err());
                if let Some(target) = materialized {
                    prop_assert!(!target.exists(), "target survived release: {target:?}");
                    // idempotent second pass
                    prop_assert!(backend.release(&meta).is_ok());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// signature: accepted names round-trip through real resources inside the
// root; rejected names fail loudly with a reason that names the input
// ---------------------------------------------------------------------

#[cfg(feature = "filesystem")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn resource_name_roundtrip_or_explained_rejection(name in unicode_string(64)) {
        // the library's rules are the contract under test; the OS has
        // opinions of its own (Windows bans `< > " | ? *`), and a name that
        // is legal by the contract can still be unwritable there
        if cfg!(windows) {
            prop_assume!(!name.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')));
        }

        let dir = TestDir::new("prop-name");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-name")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build()
            .unwrap();
        let ns = manager.namespace("named").unwrap();

        if expect_resource_name(&name) {
            let file = match ns.create_file(&name) {
                Ok(file) => file,
                Err(Error::Backend(BackendError::Io { .. })) => {
                    // the OS rejected a name the contract allows (macos
                    // EILSEQ, e.g.) — nothing contractual to pin down, skip
                    eprintln!("skipping oracle-accepted name {name:?}: the OS refused it");
                    return Ok(());
                }
                Err(e) => panic!("oracle accepted {name:?} but creation failed: {e}"),
            };
            let path = file.path().to_path_buf();
            prop_assert!(path.starts_with(manager.root()));
            prop_assert!(path.exists());
            // the positive path must stay positive on a second attempt:
            // rejected only because the file exists, never as unsafe
            match ns.create_file(&name) {
                Err(Error::Backend(BackendError::Rejected { reason, .. })) => {
                    prop_assert!(reason.contains("already exists"), "odd reason: {reason}");
                }
                other => {
                    return Err(TestCaseError::fail(format!(
                        "expected `already exists`, got {other:?}"
                    )))
                }
            }
            ns.release(file.id()).unwrap();
            prop_assert!(!path.exists());
        } else {
            match ns.create_file(&name) {
                Err(Error::Access(AccessError::UnsafeName(echo))) => {
                    prop_assert_eq!(echo.as_str(), name);
                    prop_assert!(!Error::Access(AccessError::UnsafeName(echo)).to_string().is_empty());
                }
                other => {
                    return Err(TestCaseError::fail(format!(
                        "expected UnsafeName for {name:?}, got {other:?}"
                    )))
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn namespace_name_roundtrip_or_explained_rejection(name in unicode_string(48)) {

        let dir = TestDir::new("prop-ns");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-ns")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build()
            .unwrap();

        match manager.namespace(&name) {
            Ok(ns) => {
                prop_assert!(expect_namespace_name(&name));
                prop_assert_eq!(ns.name(), name.as_str());
                // positive path must stay positive on a repeat call
                prop_assert!(manager.namespace(&name).is_ok());
                #[cfg(feature = "filesystem")]
                {
                    let ns_dir = manager.root().join(manager.instance_id()).join(&name);
                    prop_assert!(ns_dir.is_dir(), "namespace dir missing: {ns_dir:?}");
                }
            }
            Err(cachegirl::Error::Config(ConfigError::InvalidNamespaceName(echo))) => {
                prop_assert!(!expect_namespace_name(&name));
                prop_assert_eq!(echo.as_str(), name);
                prop_assert!(
                    !Error::Config(ConfigError::InvalidNamespaceName(echo))
                        .to_string()
                        .is_empty()
                );
            }
            Err(other) => {
                return Err(TestCaseError::fail(format!(
                    "expected InvalidNamespaceName for {name:?}, got {other}"
                )))
            }
        }
    }

    #[test]
    fn instance_id_roundtrip_or_explained_rejection(id in unicode_string(48)) {

        let dir = TestDir::new("prop-id");
        let built = TempManager::builder()
            .root(dir.path())
            .instance_id(&id)
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build();

        match built {
            Ok(manager) => {
                prop_assert!(expect_instance_id(&id));
                prop_assert!(!manager.is_closed());
            }
            Err(cachegirl::Error::Config(ConfigError::InvalidInstanceId(echo))) => {
                prop_assert!(!expect_instance_id(&id));
                prop_assert_eq!(echo.as_str(), id);
                prop_assert!(
                    !Error::Config(ConfigError::InvalidInstanceId(echo))
                        .to_string()
                        .is_empty()
                );
            }
            Err(other) => {
                return Err(TestCaseError::fail(format!(
                    "expected InvalidInstanceId for {id:?}, got {other}"
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------
// signature: a fixed count/byte quota never overshoots under any
// interleaving of creates and releases
// ---------------------------------------------------------------------

#[cfg(feature = "filesystem")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn count_quota_never_overshoots(
        limit in 1u64..=6,
        ops in vec(prop_oneof![3 => Just(true), 1 => Just(false)], 1..24),
    ) {
        let dir = TestDir::new("prop-quota-count");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-quota-count")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build()
            .unwrap();
        let ns = manager.namespace("count").unwrap();
        ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(limit));

        let mut live: Vec<cachegirl::ResourceId> = Vec::new();
        let mut seq = 0u32;
        for is_create in ops {
            if is_create {
                seq += 1;
                let name = format!("quota-{seq}");
                match ns.create_file(&name) {
                    Ok(file) => {
                        live.push(file.id().clone());
                        let (_bytes, count) = ns.usage().unwrap();
                        prop_assert!(count <= limit, "live count {count} > quota {limit}");
                        prop_assert_eq!(count, live.len() as u64);
                    }
                    Err(Error::Quota(cachegirl::QuotaError::CountExceeded { limit: reported })) => {
                        prop_assert_eq!(reported, limit);
                        prop_assert_eq!(live.len() as u64, limit);
                    }
                    other => {
                        return Err(TestCaseError::fail(format!(
                            "create failed with something other than the quota: {other:?}"
                        )))
                    }
                }
            } else if let Some(id) = live.pop() {
                ns.release(&id).unwrap();
                let (_bytes, count) = ns.usage().unwrap();
                prop_assert_eq!(count as usize, live.len());
            }
            let registered = manager.registry().snapshot().unwrap().len();
            prop_assert_eq!(registered, live.len());
        }
    }
}

#[cfg(feature = "memory")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn bytes_quota_never_overshoots(
        limit in prop_oneof![Just(16u64), Just(64), Just(256)],
        ops in vec((any::<bool>(), 1usize..=300), 1..20),
    ) {
        let dir = TestDir::new("prop-quota-bytes");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-quota-bytes")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build()
            .unwrap();
        let ns = manager.namespace("bytes").unwrap();
        ns.set_quota(cachegirl::NamespaceQuota::with_max_bytes(limit));

        let mut tracked_bytes = 0u64;
        let mut live: Vec<(cachegirl::ResourceId, u64)> = Vec::new();
        for (is_create, capacity) in ops {
            if is_create {
                match ns.create_buffer(capacity) {
                    Ok(buffer) => {
                        live.push((buffer.id().clone(), capacity as u64));
                        tracked_bytes = tracked_bytes.saturating_add(capacity as u64);
                        let (bytes, _count) = ns.usage().unwrap();
                        prop_assert!(bytes <= limit, "tracked bytes {bytes} > quota {limit}");
                        prop_assert_eq!(bytes, tracked_bytes);
                    }
                    Err(Error::Quota(cachegirl::QuotaError::SizeExceeded { limit: reported, .. })) => {
                        prop_assert_eq!(reported, limit);
                        prop_assert!(tracked_bytes <= limit);
                    }
                    other => {
                        return Err(TestCaseError::fail(format!(
                            "create failed with something other than the quota: {other:?}"
                        )))
                    }
                }
            } else if let Some((id, freed)) = live.pop() {
                let before = ns.usage().unwrap().0;
                ns.release(&id).unwrap();
                tracked_bytes = tracked_bytes.saturating_sub(freed);
                let (bytes, _count) = ns.usage().unwrap();
                prop_assert!(bytes < before || freed == 0);
                prop_assert_eq!(bytes, tracked_bytes);
            }
        }
    }
}

// ---------------------------------------------------------------------
// signature: `is_expired` is monotone in `now`, expiry kicks in exactly
// at the boundary
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn ttl_is_expired_is_monotone_in_now(
        created_off in -10_000_000i64..10_000_000,
        accessed_off in -10_000_000i64..10_000_000,
        ttl_secs in 0u64..1_000_000,
        dt1 in 0u64..100_000,
        dt2 in 0u64..100_000,
        idle in any::<bool>(),
    ) {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(40_000_000);
        let offset = |off: i64| {
            if off >= 0 {
                base + Duration::from_secs(off as u64)
            } else {
                base - Duration::from_secs((-off) as u64)
            }
        };
        let created = offset(created_off);
        let accessed = offset(accessed_off);
        let ttl = if idle {
            Ttl::Idle(Duration::from_secs(ttl_secs))
        } else {
            Ttl::Fixed(Duration::from_secs(ttl_secs))
        };

        let now1 = base + Duration::from_secs(dt1);
        let now2 = now1 + Duration::from_secs(dt2);
        if ttl.is_expired(created, accessed, now1) {
            prop_assert!(
                ttl.is_expired(created, accessed, now2),
                "expired at {now1:?} but not at {now2:?}"
            );
        }

        // exactly at the boundary counts as expired; one millisecond before does not
        let anchor = if idle { accessed } else { created };
        let exact = anchor + Duration::from_secs(ttl_secs);
        prop_assert!(ttl.is_expired(created, accessed, exact));
        if ttl_secs > 0 {
            prop_assert!(!ttl.is_expired(created, accessed, exact - Duration::from_millis(1)));
        }
    }
}

// ---------------------------------------------------------------------
// signature: the state machine never accepts an illegal move, never
// panics on a legal-looking one, and `Destroyed` stays terminal
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn try_transition_matches_the_predicate(
        from in (0u8..10).prop_map(state_by_index),
        to in (0u8..10).prop_map(state_by_index),
    ) {
        let legal = from.can_transition_to(to);
        match from.try_transition(to) {
            Ok(next) => {
                prop_assert!(legal);
                prop_assert_eq!(next, to);
            }
            Err(cachegirl::RegistryError::InvalidTransition { from: reported_from, to: reported_to }) => {
                prop_assert!(!legal);
                prop_assert_eq!(reported_from, from);
                prop_assert_eq!(reported_to, to);
            }
            other => {
                return Err(TestCaseError::fail(format!("unexpected error: {other:?}")))
            }
        }
    }

    #[test]
    fn self_transitions_are_always_rejected(from in (0u8..10).prop_map(state_by_index)) {
        prop_assert!(!from.can_transition_to(from));
        prop_assert!(from.try_transition(from).is_err());
    }

    #[test]
    fn destroyed_is_a_hard_sink(to in (0u8..10).prop_map(state_by_index)) {
        prop_assert!(ResourceState::Destroyed.try_transition(to).is_err());
    }

    #[test]
    fn random_walk_never_leaves_the_reachable_states(
        walk in vec(0u8..10, 1..200),
    ) {
        let mut state = ResourceState::Uninitialized;
        let mut destroyed_seen = false;
        for raw in walk {
            let target = state_by_index(raw);
            match state.try_transition(target) {
                Ok(next) => {
                    // once Destroyed, no transition of any kind is ever accepted
                    prop_assert!(
                        !destroyed_seen,
                        "accepted a transition out of terminal Destroyed"
                    );
                    prop_assert!(state.can_transition_to(target));
                    state = next;
                }
                Err(cachegirl::RegistryError::InvalidTransition { .. }) => {
                    prop_assert!(!state.can_transition_to(target));
                }
                other => {
                    return Err(TestCaseError::fail(format!("unexpected error: {other:?}")))
                }
            }
            if state == ResourceState::Destroyed {
                destroyed_seen = true;
            }
        }
    }
}

fn state_by_index(i: u8) -> ResourceState {
    const ALL: [ResourceState; 10] = [
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
    ];
    ALL[i as usize % ALL.len()]
}

// ---------------------------------------------------------------------
// signature: the cleanup report keeps its promises on real passes, and
// `RemovalReasons::destroyed` is exactly the sum of the removal causes
// ---------------------------------------------------------------------

// a backend that always succeeds and never touches the disk, so report
// arithmetic can be checked without the filesystem
struct NoopBackend;

impl cachegirl::backend::ResourceBackend for NoopBackend {
    fn name(&self) -> &str {
        "noop"
    }

    fn create(
        &self,
        request: &cachegirl::backend::ResourceRequest,
    ) -> cachegirl::Result<ResourceMeta> {
        let mut meta = ResourceMeta::new(&request.name, &request.namespace, request.kind);
        meta.size_bytes = request.size_hint.unwrap_or(0);
        meta.metadata
            .insert("backend".to_string(), "noop".to_string());
        Ok(meta)
    }

    fn release(&self, _meta: &ResourceMeta) -> cachegirl::Result<()> {
        Ok(())
    }
}

fn meta_with(name: &str, bytes: u64, ttl: Option<Ttl>, backdated_secs: u64) -> ResourceMeta {
    let mut meta = ResourceMeta::new(name, "report", ResourceKind::Buffer);
    meta.size_bytes = bytes;
    meta.ttl = ttl;
    meta.metadata
        .insert("backend".to_string(), "noop".to_string());
    let past = SystemTime::now() - Duration::from_secs(backdated_secs);
    meta.created_at = past;
    meta.last_accessed = past;
    meta
}

fn backdated_expired(name: &str, bytes: u64) -> ResourceMeta {
    meta_with(name, bytes, Some(Ttl::Fixed(Duration::from_secs(1))), 100)
}

fn live_managed(name: &str, bytes: u64) -> ResourceMeta {
    meta_with(name, bytes, None, 0)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn cleanup_report_arithmetic_holds_on_real_reports(
        n_expired in 0usize..12,
        n_live in 0usize..12,
        sizes in vec(0u64..10_000, 1..24),
    ) {
        let dir = TestDir::new("prop-report");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-report")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .build()
            .unwrap();
        manager.register_backend(Arc::new(NoopBackend)).unwrap();
        let registry = manager.registry();

        for i in 0..n_expired {
            let size = sizes[i % sizes.len()];
            registry
                .register(backdated_expired(&format!("expired-{i}"), size))
                .unwrap();
        }
        for i in 0..n_live {
            let size = sizes[(n_expired + i) % sizes.len()];
            registry
                .register(live_managed(&format!("live-{i}"), size))
                .unwrap();
        }

        let expected_bytes = (0..n_expired).fold(0u64, |acc, i| {
            acc.saturating_add(sizes[i % sizes.len()])
        });
        let report = manager.cleanup_expired().unwrap();

        prop_assert_eq!(report.scanned, (n_expired + n_live) as u64);
        prop_assert_eq!(report.removed, n_expired as u64);
        prop_assert_eq!(report.failed, 0);
        prop_assert_eq!(report.removed, report.reasons.destroyed());
        prop_assert_eq!(report.deferred, report.reasons.lease_deferred);
        prop_assert_eq!(report.bytes_freed, expected_bytes);
        // every scanned resource ends up accounted for; the sweep phase
        // charges exactly one outcome per candidate, the quota phases may
        // charge more — never less
        prop_assert!(
            report.removed + report.skipped + report.failed + report.deferred >= report.scanned,
            "report lost track of resources: {report:?}"
        );
    }

    #[test]
    fn removal_reasons_destroyed_sums_the_removal_causes(
        ttl_fixed in 0u64..1_000_000,
        ttl_idle in 0u64..1_000_000,
        size_eviction in 0u64..1_000_000,
        count_eviction in 0u64..1_000_000,
        policy in 0u64..1_000_000,
        lease_deferred in 0u64..1_000_000,
        eligible in 0u64..1_000_000,
    ) {
        let reasons = cachegirl::RemovalReasons {
            ttl_fixed,
            ttl_idle,
            size_eviction,
            count_eviction,
            policy,
            lease_deferred,
            eligible,
        };
        prop_assert_eq!(
            reasons.destroyed(),
            ttl_fixed + ttl_idle + size_eviction + count_eviction + policy + eligible
        );
        prop_assert_eq!(
            reasons.is_quiet(),
            reasons.destroyed() == 0 && lease_deferred == 0
        );
    }
}

// ---------------------------------------------------------------------
// signature: builder combinations assemble arithmetically, no panic at
// u64::MAX sizes anywhere in the quota math
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn cleanup_policy_builder_roundtrips_and_stays_deterministic(
        ttl_kind in 0u8..3,
        ttl_secs in 0u64..1_000_000,
        max_size in proptest::option::of(any::<u64>()),
        max_resources in proptest::option::of(any::<u64>()),
        priority in (0u8..4).prop_map(|i| {
            [Priority::Low, Priority::Normal, Priority::High, Priority::Critical][i as usize]
        }),
    ) {
        let build = || {
            let mut builder = CleanupPolicy::builder();
            match ttl_kind {
                0 => {}
                1 => builder = builder.ttl(Duration::from_secs(ttl_secs)),
                _ => builder = builder.idle_ttl(Duration::from_secs(ttl_secs)),
            }
            if let Some(bytes) = max_size {
                builder = builder.max_size(bytes);
            }
            if let Some(count) = max_resources {
                builder = builder.max_resources(count);
            }
            builder.priority(priority).build()
        };
        let first = build();
        let second = build();

        prop_assert_eq!(first, second);
        prop_assert_eq!(first.quota.max_bytes, max_size);
        prop_assert_eq!(first.quota.max_resources, max_resources);
        prop_assert_eq!(first.priority, priority);
        match ttl_kind {
            0 => prop_assert!(first.ttl.is_none()),
            1 => prop_assert_eq!(first.ttl, Some(Ttl::Fixed(Duration::from_secs(ttl_secs)))),
            _ => prop_assert_eq!(first.ttl, Some(Ttl::Idle(Duration::from_secs(ttl_secs)))),
        }
    }

    #[test]
    fn quota_evaluation_saturates_instead_of_panicking(
        current_bytes in any::<u64>(),
        current_registers in any::<u64>(),
        added_bytes in any::<u64>(),
        max_bytes in proptest::option::of(any::<u64>()),
        max_resources in proptest::option::of(any::<u64>()),
    ) {
        let quota = Quota { max_bytes, max_resources };
        let _ = quota.allows(current_bytes, current_registers, added_bytes);

        let ns_quota = cachegirl::NamespaceQuota { max_bytes, max_resources };
        // must not panic; overflow panics are the failure mode here
        let _ = ns_quota.check("ns", current_bytes, current_registers, added_bytes);
    }

    #[test]
    fn cleanup_pass_with_absurd_sizes_does_not_panic(
        sizes in vec(prop_oneof![Just(0u64), Just(1), Just(u64::MAX - 1), Just(u64::MAX)], 1..5),
    ) {
        let dir = TestDir::new("prop-absurd");
        let manager = TempManager::builder()
            .root(dir.path())
            .instance_id("prop-absurd")
            .recovery_policy(cachegirl::RecoveryPolicy::Never)
            .max_size(u64::MAX)
            .build()
            .unwrap();
        manager.register_backend(Arc::new(NoopBackend)).unwrap();
        let registry = manager.registry();

        for (i, size) in sizes.iter().enumerate() {
            registry
                .register(backdated_expired(&format!("absurd-{i}"), *size))
                .unwrap();
        }
        let report = manager.cleanup_expired().unwrap();
        prop_assert_eq!(report.removed, sizes.len() as u64);
        prop_assert_eq!(report.failed, 0);
    }
}

// ---------------------------------------------------------------------
// signature: interleaved register/remove from several threads leaves
// the registry consistent with what survived
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn registry_stays_consistent_under_interleaved_ops(
        ops in vec((0u8..4, 0u8..2, 0u8..16), 8..48),
    ) {
        let metas: Vec<ResourceMeta> = (0..16)
            .map(|i| ResourceMeta::new(format!("meta-{i}"), "thr", ResourceKind::Buffer))
            .collect();
        let registry = Arc::new(Registry::new());

        // deal the ops out round-ish by thread id; intra-thread order is preserved
        let mut per_thread: Vec<Vec<(bool, usize)>> = vec![Vec::new(); 4];
        for (thread, op, index) in ops {
            per_thread[thread as usize].push((op == 0, index as usize));
        }
        let handles: Vec<_> = per_thread
            .into_iter()
            .map(|my_ops| {
                let registry = Arc::clone(&registry);
                let metas = metas.clone();
                std::thread::spawn(move || {
                    for (is_register, index) in my_ops {
                        if is_register {
                            let _ = registry.register(metas[index].clone());
                        } else {
                            let _ = registry.remove(&metas[index].id);
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let len = registry.len().unwrap();
        let snapshot = registry.snapshot().unwrap();
        prop_assert_eq!(len, snapshot.len());

        // no id registered twice
        let mut ids: Vec<_> = snapshot.iter().map(|m| m.id.clone()).collect();
        ids.sort();
        ids.dedup();
        prop_assert_eq!(ids.len(), snapshot.len());

        // byte accounting matches the records behind the same lock-free moment
        let bytes = registry.total_bytes().unwrap();
        prop_assert_eq!(
            bytes,
            snapshot.iter().fold(0u64, |acc, m| acc.saturating_add(m.size_bytes))
        );

        // every record in the snapshot is fetchable and self-consistent
        for meta in &snapshot {
            let got = registry.get(&meta.id).unwrap();
            prop_assert_eq!(got.namespace, meta.namespace.as_str());
        }
    }
}
