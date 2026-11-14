//! Security attack scenarios for the filesystem backend and the validators.
//!
//! Everything here is a real attack against real file operations; nothing
//! is mocked. Each test plants the attack the library would see in the
//! wild (a hostile name, a tampered registry record, a planted symlink)
//! and then asserts the confinement property that must hold no matter
//! what: the manager's root is the boundary, and nothing outside it may
//! be read, written or deleted because of an API call.
//!
//! Where a test plants a symlink, it skips (does not fail) when the
//! platform refuses to create one — e.g. Windows without the symlink
//! privilege. The skip is loud on purpose: the property then goes
//! unverified on that machine, not silently assumed.

#![cfg(feature = "filesystem")]

mod common;

use std::path::{Path, PathBuf};

use cachegirl::backend::{FilesystemBackend, ResourceBackend, ResourceRequest};
use cachegirl::{
    AccessError, BackendError, ConfigError, Error, QuotaError, ResourceKind, ResourceMeta,
    ResourceState, SymlinkPolicy, TempManager,
};
use common::TestDir;

#[cfg(unix)]
fn try_symlink_file(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(unix)]
fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn try_symlink_file(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

#[cfg(windows)]
fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

fn build_manager(root: &Path, instance_id: &str) -> TempManager {
    TempManager::builder()
        .root(root)
        .instance_id(instance_id)
        .build()
        .unwrap()
}

fn backend(root: &Path) -> FilesystemBackend {
    FilesystemBackend::new(root, "instance-sec", SymlinkPolicy::Reject).unwrap()
}

fn symlink_or_skip(link: &Path) -> bool {
    if link.symlink_metadata().is_ok() {
        return true;
    }
    eprintln!("symlinks unavailable on this machine; symlink attack test skipped");
    false
}

// -- 1. path traversal in resource names ------------------------------------

#[test]
fn traversal_names_are_rejected_everywhere() {
    // the whole point: a resource name must never be able to spell a path
    let root = TestDir::new("sec-traversal");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let be = backend(root.path());

    let attacks = [
        "../..",
        "..\\..",
        "..../",
        "../../evil",
        "..\\..\\evil",
        "x/../../evil",
        "x\\..\\..\\evil",
        "ns/../other",
        "ns\\..\\other",
        "/etc/passwd",
        "C:\\Windows\\evil.dll",
        "C:/Windows/evil.dll",
        "\\\\server\\share\\evil",
        "....",
        ".",
        "..",
        "name.",
        "trailing-dot.",
        "   ",
        " padded",
        "padded ",
        "a:b",
        "a/b",
        "a\\b",
    ];
    for attack in attacks {
        let via_manager = ns.create_file(attack);
        assert!(
            matches!(via_manager, Err(Error::Access(AccessError::UnsafeName(_)))),
            "manager must reject {attack:?}"
        );
        // the backend validates again on its own: even a caller that
        // skips the manager's checks gets nothing past it
        let request = ResourceRequest::new(attack, "ns", ResourceKind::File);
        assert!(
            be.create(&request).is_err(),
            "backend must reject {attack:?}"
        );
    }
    manager.shutdown().unwrap();
}

#[test]
fn traversal_via_backend_namespace_parameter_is_rejected() {
    // the namespace field of a request is a path too; no segment may
    // escape the instance dir. A legal nested path (`a/b`) is accepted
    // and materializes under root/<instance>/a/b/ confined, by design.
    let root = TestDir::new("sec-ns-escape");
    let be = backend(root.path());

    for namespace in [
        "ns/../other",
        "ns\\..\\other",
        "..",
        ".",
        "a//b",
        "/a",
        "a/",
        "a\\b",
    ] {
        let request = ResourceRequest::new("ok-name", namespace, ResourceKind::File);
        assert!(
            be.create(&request).is_err(),
            "backend must reject namespace {namespace:?}"
        );
    }

    // a legal nested path creates the whole subtree, confined
    let request = ResourceRequest::new("ok-name", "a/b", ResourceKind::File);
    let meta = be.create(&request).expect("nested namespace is legal");
    let path = std::path::PathBuf::from(meta.metadata.get("path").unwrap());
    assert!(path.starts_with(be.root()));
    let rel = path.strip_prefix(be.root()).unwrap();
    assert!(
        rel.ends_with("a/b/ok-name".replace('/', std::path::MAIN_SEPARATOR_STR)),
        "nested file must land in the subtree: {rel:?}"
    );
}

#[test]
fn namespace_escape_variants_are_rejected_at_the_api() {
    // whoever hands the library a weird namespace name gets an error,
    // not a directory outside the instance
    let root = TestDir::new("sec-ns-api-escape");
    let manager = build_manager(root.path(), "instance-sec");

    for namespace in ["ns/../other", "ns\\..\\other", "..", ".", "ns.", "../x"] {
        assert!(
            manager.namespace(namespace).is_err(),
            "namespace {namespace:?}"
        );
    }
    manager.shutdown().unwrap();
}

#[test]
fn unicode_fullwidth_separators_stay_literal() {
    // U+FF3C (fullwidth reverse solidus) and U+FF0F look like the
    // real separators but are ordinary filename characters on Windows
    // and Linux: the OS never splits on them, so the only safe reading
    // of such a name is a literal file with that exact name, confined
    // to the namespace dir. Documented by pinning the behavior: if a
    // platform ever grows these into separators, this test must fail.
    let root = TestDir::new("sec-fullwidth");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let instance_dir = std::fs::canonicalize(root.path().join("instance-sec").join("ns")).unwrap();
    for tricky in [
        "..\u{ff3c}..\u{ff3c}escape.bin",
        "..\u{ff0f}..\u{ff0f}escape.bin",
        "\u{2024}\u{2024}", // two one-dot leaders: looks like "..", is not
        "\u{ff0e}\u{ff0e}", // two fullwidth full stops: same story
    ] {
        let file = ns
            .create_file(tricky)
            .unwrap_or_else(|e| panic!("name {tricky:?} should be a literal file: {e}"));
        assert!(
            file.path().starts_with(&instance_dir),
            "literal-name file must stay inside the namespace dir"
        );
        assert_eq!(
            file.path().file_name().unwrap().to_str().unwrap(),
            tricky,
            "the OS must not rewrite the name either"
        );
    }
    manager.shutdown().unwrap();
}

#[test]
fn windows_reserved_device_names_are_rejected() {
    // "NUL" and friends are not filenames on Windows: the call opens
    // the device, the write goes nowhere, the registry believes a file
    // exists. Rejecting them beats an alias that lies.
    let root = TestDir::new("sec-devices");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let be = backend(root.path());

    for reserved in [
        "NUL", "nul", "NUL.txt", "CON", "con.log", "AUX", "PRN", "COM1", "COM9.bin", "LPT7",
    ] {
        assert!(
            matches!(
                ns.create_file(reserved),
                Err(Error::Access(AccessError::UnsafeName(_)))
            ),
            "reserved device name {reserved:?} must be rejected"
        );
        let request = ResourceRequest::new(reserved, "ns", ResourceKind::File);
        assert!(
            be.create(&request).is_err(),
            "backend must reject {reserved:?}"
        );
    }
    // near misses are fine: they are ordinary names
    ns.create_file("COM10").unwrap();
    ns.create_file("null-device").unwrap();
    manager.shutdown().unwrap();
}

#[test]
fn names_with_control_characters_are_rejected() {
    // control chars smuggle newlines into logs and corrupt lock/key
    // formats; NTFS famously rejects them anyway
    let root = TestDir::new("sec-control");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let be = backend(root.path());

    for dirty in [
        "bad\0name",
        "bad\u{7f}name",
        "bad\u{1f}name",
        "\0",
        "bad\ttab",
    ] {
        assert!(
            ns.create_file(dirty).is_err(),
            "control-char name must be rejected: {dirty:?}"
        );
        let request = ResourceRequest::new(dirty, "ns", ResourceKind::File);
        assert!(
            be.create(&request).is_err(),
            "backend must reject {dirty:?}"
        );
    }
    manager.shutdown().unwrap();
}

#[test]
fn oversized_names_error_instead_of_panicking() {
    // both exceed NAME_MAX / MAX_PATH; the library must surface a typed
    // error and leave the books clean, never panic or truncate
    let root = TestDir::new("sec-big-name");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();

    for huge in ["x".repeat(4096), "y".repeat(10 * 1024)] {
        let err = ns.create_file(&huge).unwrap_err();
        assert!(
            matches!(err, Error::Backend(_)),
            "an oversized name must be a typed backend error, got {err:?}"
        );
        assert!(!err.to_string().is_empty());
    }
    let stats = manager.stats().unwrap();
    assert_eq!(
        stats.resources, 0,
        "failed creates must not leak into stats"
    );
    manager.shutdown().unwrap();
}

// -- 1b. instance id injection ----------------------------------------------

#[test]
fn build_rejects_dangerous_instance_ids() {
    // the instance id becomes a directory directly under the root and a
    // lock-file path; "." or ".." would alias the root itself or climb
    // above it, separators would fan out anywhere
    let root = TestDir::new("sec-instance-id");

    for evil in ["../..", "..", ".", "a/b", "a\\b", "trailing.", "  "] {
        let result = TempManager::builder()
            .root(root.path())
            .instance_id(evil)
            .build();
        assert!(
            matches!(
                result,
                Err(Error::Config(ConfigError::InvalidInstanceId(_)))
            ),
            "instance id {evil:?} must be rejected"
        );
    }

    // an ordinary id is fine, and the manager actually comes up
    let ok = TempManager::builder()
        .root(root.path())
        .instance_id("instance-ok_1.2")
        .build()
        .unwrap();
    ok.shutdown().unwrap();
}

// -- 2. symlink attacks ------------------------------------------------------

#[test]
fn symlinked_file_at_final_path_is_rejected_on_create() {
    // attacker pre-plants the exact name the manager will create, as a
    // symlink to a foreign file; the Reject policy must refuse before a
    // single byte is written through it
    let root = TestDir::new("sec-link-final");
    let outside = TestDir::new("sec-link-final-out");
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, b"precious").unwrap();

    let manager = build_manager(root.path(), "instance-sec");
    let ns_dir = root.path().join("instance-sec").join("ns");
    std::fs::create_dir_all(&ns_dir).unwrap();
    let link = ns_dir.join("planted.bin");
    if !(try_symlink_file(&victim, &link) && symlink_or_skip(&link)) {
        manager.shutdown().unwrap();
        return;
    }

    let ns = manager.namespace("ns").unwrap();
    let err = ns.create_file("planted.bin").unwrap_err();
    assert!(
        matches!(err, Error::Access(AccessError::SymlinkRejected(_))),
        "planted symlink must be refused by policy, got {err:?}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    manager.shutdown().unwrap();
}

#[test]
fn release_of_symlink_never_deletes_the_target() {
    // the final component is a symlink to a foreign file: release may
    // remove the link itself, but the target must survive untouched
    let root = TestDir::new("sec-link-release");
    let outside = TestDir::new("sec-link-release-out");
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, b"precious").unwrap();

    let ns_dir = root.path().join("instance-sec").join("ns");
    std::fs::create_dir_all(&ns_dir).unwrap();
    let link = ns_dir.join("planted.bin");
    if !(try_symlink_file(&victim, &link) && symlink_or_skip(&link)) {
        return;
    }

    let be = backend(root.path());
    // keep the lexical path under the backend's (canonical) root style,
    // exactly as a real record written by the backend would look
    let canon_root = std::fs::canonicalize(root.path()).unwrap();
    let shadow_link = canon_root
        .join("instance-sec")
        .join("ns")
        .join("planted.bin");
    let mut meta = ResourceMeta::new("planted.bin", "ns", ResourceKind::File);
    meta.metadata
        .insert("path".into(), shadow_link.to_string_lossy().into_owned());
    be.release(&meta).unwrap();

    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert!(
        link.symlink_metadata().is_err(),
        "the link itself is disposable; the target must survive"
    );
}

#[test]
fn symlinked_namespace_dir_never_swallows_created_files() {
    // the namespace dir itself is planted as a symlink out of the root;
    // creating a file through it would silently write foreign disk.
    // unfixed, the backend only checks the final component and the file
    // lands outside the root — this is the attack that matters.
    let root = TestDir::new("sec-link-parent");
    let outside = TestDir::new("sec-link-parent-out");

    let instance_dir = root.path().join("instance-sec");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let ns_dir = instance_dir.join("ns");
    if !(try_symlink_dir(outside.path(), &ns_dir) && symlink_or_skip(&ns_dir)) {
        return;
    }

    let manager = TempManager::builder()
        .root(root.path())
        .instance_id("instance-sec")
        .recovery_policy(cachegirl::RecoveryPolicy::Never)
        .build()
        .unwrap();
    let ns = manager.namespace("ns").unwrap();
    let result = ns.create_file("loot.bin");
    assert!(
        result.is_err(),
        "a namespace dir that resolves outside the root must not be used"
    );
    assert!(
        !outside.path().join("loot.bin").exists(),
        "nothing may be created outside the root through a symlinked parent"
    );
    manager.shutdown().unwrap();
}

#[test]
fn release_through_symlinked_parent_is_refused() {
    // same plant, viewed from the delete side: a meta whose lexically
    // confined path walks out through a symlinked namespace dir must not
    // authorize deleting the foreign file it resolves to
    let root = TestDir::new("sec-link-parent-rel");
    let outside = TestDir::new("sec-link-parent-rel-out");
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, b"precious").unwrap();

    let instance_dir = root.path().join("instance-sec");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let ns_dir = instance_dir.join("ns");
    if !(try_symlink_dir(outside.path(), &ns_dir) && symlink_or_skip(&ns_dir)) {
        return;
    }

    // a meta whose lexical path stays under the canonical root, but
    // whose parent component is the planted symlink — only the ancestor
    // check can catch this one
    let canon_root = std::fs::canonicalize(root.path()).unwrap();
    let shadow_path = canon_root
        .join("instance-sec")
        .join("ns")
        .join("victim.bin");
    let be = backend(root.path());
    let mut meta = ResourceMeta::new("victim.bin", "ns", ResourceKind::File);
    meta.metadata
        .insert("path".into(), shadow_path.to_string_lossy().into_owned());
    let err = be.release(&meta).unwrap_err();
    assert!(
        matches!(err, Error::Access(AccessError::OutsideRoot { .. })),
        "release through a foreign parent must be refused, got {err:?}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn symlink_policy_within_root_allows_local_links_but_refuses_foreign_ones() {
    // WithinRoot is the middle setting: links are fine as long as they
    // resolve inside the manager's root; a dangling link cannot be
    // verified and is refused as well
    let root = TestDir::new("sec-link-within");
    let outside = TestDir::new("sec-link-within-out");
    let foreign_target = outside.path().join("foreign.bin");
    std::fs::write(&foreign_target, b"precious").unwrap();
    let local_target = root.path().join("local-target.bin");
    std::fs::write(&local_target, b"local").unwrap();

    let instance_dir = root.path().join("instance-sec");
    let ns_dir = instance_dir.join("ns");
    std::fs::create_dir_all(&ns_dir).unwrap();
    let local_link = ns_dir.join("local-link.bin");
    let foreign_link = ns_dir.join("foreign-link.bin");
    let planted = try_symlink_file(&local_target, &local_link)
        && try_symlink_file(&foreign_target, &foreign_link);
    if !planted {
        eprintln!("symlinks unavailable on this machine; WithinRoot test skipped");
        return;
    }

    let within =
        FilesystemBackend::new(root.path(), "instance-sec", SymlinkPolicy::WithinRoot).unwrap();

    // local link: passes the symlink policy; creation then fails for the
    // ordinary reason that create_new refuses to clobber an existing file
    let request = ResourceRequest::new("local-link.bin", "ns", ResourceKind::File);
    let err = within.create(&request).unwrap_err();
    assert!(
        !matches!(
            err,
            Error::Access(AccessError::OutsideRoot { .. })
                | Error::Access(AccessError::SymlinkRejected(_))
        ),
        "a link resolving inside the root must pass the policy, got {err:?}"
    );

    // foreign link: refused by the policy before any file is touched
    let request = ResourceRequest::new("foreign-link.bin", "ns", ResourceKind::File);
    let err = within.create(&request).unwrap_err();
    assert!(
        matches!(err, Error::Access(AccessError::OutsideRoot { .. })),
        "a link resolving outside the root must be refused, got {err:?}"
    );
    assert_eq!(std::fs::read(&foreign_target).unwrap(), b"precious");
}

#[test]
fn dangling_symlink_is_rejected_under_both_policies() {
    // a link whose target does not exist cannot be verified against the
    // root; refusing beats guessing
    let root = TestDir::new("sec-link-dangling");
    let instance_dir = root.path().join("instance-sec");
    let ns_dir = instance_dir.join("ns");
    std::fs::create_dir_all(&ns_dir).unwrap();

    let missing_target = root.path().join("no-such-target.bin");
    let dangling = ns_dir.join("dangling.bin");
    if !(try_symlink_file(&missing_target, &dangling) && symlink_or_skip(&dangling)) {
        return;
    }

    let reject = backend(root.path());
    let request = ResourceRequest::new("dangling.bin", "ns", ResourceKind::File);
    assert!(matches!(
        reject.create(&request),
        Err(Error::Access(AccessError::SymlinkRejected(_)))
    ));

    let within =
        FilesystemBackend::new(root.path(), "instance-sec", SymlinkPolicy::WithinRoot).unwrap();
    assert!(matches!(
        within.create(&request),
        Err(Error::Access(AccessError::OutsideRoot { .. }))
    ));
}

// -- 3. tampered metadata ----------------------------------------------------

#[test]
fn backend_release_with_tampered_outside_path_is_refused() {
    // a registry record (or any hand-assembled meta) that points at a
    // file outside the root must never authorize deleting that file
    let root = TestDir::new("sec-tamper-release");
    let outside = TestDir::new("sec-tamper-release-out");
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, b"precious").unwrap();

    let be = backend(root.path());
    let mut meta = ResourceMeta::new("victim.bin", "ns", ResourceKind::File);
    meta.metadata
        .insert("path".into(), victim.to_string_lossy().into_owned());
    let err = be.release(&meta).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Access(AccessError::OutsideRoot { .. })
                | Error::Backend(BackendError::PathEscape(_))
        ),
        "tampered path must be refused, got {err:?}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");

    // same attack, path routed through ".." so the leading component is
    // the root itself: normalization has to resolve it before the check
    let escape = root
        .path()
        .join("..")
        .join(outside.path().file_name().unwrap())
        .join("victim.bin");
    let mut meta = ResourceMeta::new("victim.bin", "ns", ResourceKind::File);
    meta.metadata
        .insert("path".into(), escape.to_string_lossy().into_owned());
    assert!(be.release(&meta).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

// -- 7. delete confinement through cleanup -----------------------------------

#[test]
fn cleanup_never_deletes_a_file_pointed_at_by_tampered_meta() {
    // full-strength attack: create a real resource, then tamper its
    // registry record so the recorded path is a foreign file;
    // cleanup must fail the record, not delete the foreign file
    let root = TestDir::new("sec-cleanup-confinement");
    let outside = TestDir::new("sec-cleanup-confinement-out");
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, b"precious").unwrap();

    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let file = ns.create_file("tracked.bin").unwrap();
    let id = file.id().clone();
    let inner_path = file.path().to_path_buf();
    drop(file);

    // the tampered record now points at the foreign victim
    let victim_str = victim.to_string_lossy().into_owned();
    manager
        .registry()
        .update(&id, |m| {
            m.metadata.insert("path".into(), victim_str.clone());
        })
        .unwrap();
    manager
        .registry()
        .set_state(&id, ResourceState::Expired)
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(
        report.removed, 0,
        "the tampered record must not be destroyed"
    );
    assert_eq!(report.failed, 1, "the failed destruction is reported");
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    // the real file inside the root is also left alone: release bailed
    // before touching anything
    assert!(inner_path.exists());
    let record = manager.registry().get(&id).unwrap();
    assert_eq!(record.state, ResourceState::FailedCleanup);

    // explicit release goes through the same refusal
    let err = ns.release(&id).unwrap_err();
    assert!(
        matches!(err, Error::Access(AccessError::OutsideRoot { .. })),
        "release must refuse the tampered path, got {err:?}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    manager.shutdown().unwrap();
}

// -- 5. oversized inputs and quota arithmetic --------------------------------

#[test]
fn registry_records_with_u64_max_size_do_not_panic_quota_checks() {
    // a hand-assembled record can carry size_bytes == u64::MAX; the
    // projection math must saturate instead of overflowing, in both
    // the limited and the unlimited configuration
    let root = TestDir::new("sec-max-size");
    let limited = TempManager::builder()
        .root(root.path())
        .instance_id("instance-limited")
        .max_size(1024)
        .build()
        .unwrap();
    let ns = limited.namespace("ns").unwrap();
    let file = ns.create_file("inflated.bin").unwrap();
    let id = file.id().clone();
    limited
        .registry()
        .update(&id, |m| m.size_bytes = u64::MAX)
        .unwrap();

    // limited: projection saturates and refuses the next create cleanly
    let err = ns.create_file("next.bin").unwrap_err();
    assert!(
        matches!(err, Error::Quota(QuotaError::SizeExceeded { .. })),
        "saturated quota must reject with SizeExceeded, got {err:?}"
    );
    limited.shutdown().unwrap();

    // unlimited: no projection is applied at all; nothing panics
    let root2 = TestDir::new("sec-max-size-open");
    let open = build_manager(root2.path(), "instance-open");
    let ns = open.namespace("ns").unwrap();
    let file = ns.create_file("inflated.bin").unwrap();
    open.registry()
        .update(file.id(), |m| m.size_bytes = u64::MAX)
        .unwrap();
    ns.create_file("next.bin").unwrap();
    let _ = open.stats().unwrap();
    open.shutdown().unwrap();
}

#[test]
fn cleanup_eviction_arithmetic_saturates_on_huge_records() {
    // eviction and byte accounting must survive a u64::MAX tracked size:
    // saturation, not a debug-build overflow panic
    let root = TestDir::new("sec-eviction-saturation");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();
    let file = ns.create_file("inflated.bin").unwrap();
    let id = file.id().clone();
    manager
        .registry()
        .update(&id, |m| m.size_bytes = u64::MAX)
        .unwrap();
    manager
        .registry()
        .set_state(&id, ResourceState::Expired)
        .unwrap();

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.bytes_freed, u64::MAX, "byte accounting saturates");
    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn memory_backend_accounts_u64_max_hints_without_overflowing() {
    // backends see size_hint straight from requests; u64::MAX must be
    // accounted with saturation, never with an overflowing sum
    use cachegirl::backend::MemoryBackend;

    let unlimited = MemoryBackend::new();
    let hint = u64::MAX;
    for _ in 0..2 {
        let request =
            ResourceRequest::new("giant", "ns", ResourceKind::Buffer).with_size_hint(hint);
        unlimited.create(&request).unwrap();
    }
    assert_eq!(unlimited.current_bytes(), u64::MAX);
    assert_eq!(unlimited.tracked(), 2);

    // with the u64::MAX cap the second u64::MAX hint still fits by
    // saturation; a cap of u64::MAX - 1 refuses it cleanly
    let maxed = MemoryBackend::with_max_bytes(u64::MAX);
    for _ in 0..2 {
        let request =
            ResourceRequest::new("giant", "ns", ResourceKind::Buffer).with_size_hint(u64::MAX);
        maxed.create(&request).unwrap();
    }
    assert_eq!(maxed.current_bytes(), u64::MAX);

    let tight = MemoryBackend::with_max_bytes(u64::MAX - 1);
    let request =
        ResourceRequest::new("first", "ns", ResourceKind::Buffer).with_size_hint(u64::MAX);
    let err = tight.create(&request).unwrap_err();
    assert!(matches!(err, Error::Quota(QuotaError::SizeExceeded { .. })));
}

// -- 9. lock-file attacks ----------------------------------------------------

fn write_manual_lock(dir: &Path, lines: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut body = String::from("# cachegirl instance lock — do not edit\nversion=2\n");
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(dir.join(".cachegirl.lock"), body).unwrap();
}

#[test]
fn lock_naming_a_live_foreign_pid_is_never_swept() {
    // the attack: replace an instance's lock with one naming the pid of
    // a live foreign process (here: this very process, the scenario's
    // own "текущего!" case). With any consistent token the directory
    // must survive the sweep untouched.
    let root = TestDir::new("sec-lock-live");
    let squatter = root.path().join("instance-squatter");
    write_manual_lock(
        &squatter,
        &[&format!("pid={}", std::process::id())], // no token: liveness fallback
    );
    std::fs::write(squatter.join("precious.bin"), b"keep me").unwrap();

    let manager = cachegirl::RecoveryManager::new(root.path());
    let report = manager
        .recover(cachegirl::RecoveryPolicy::OnStartup, "instance-current")
        .unwrap();
    assert_eq!(report.removed, 0, "a live foreign pid must not be swept");
    assert_eq!(report.kept, 1);
    assert!(squatter.join("precious.bin").exists());

    // and with this process's real token (what write_lock would write
    // for us right now): also kept, obviously
    let root2 = TestDir::new("sec-lock-live-token");
    let manager2 = cachegirl::RecoveryManager::new(root2.path());
    manager2.write_lock("instance-squatter").unwrap();
    std::fs::write(
        root2.path().join("instance-squatter").join("precious.bin"),
        b"keep me",
    )
    .unwrap();
    let report = manager2
        .recover(cachegirl::RecoveryPolicy::OnStartup, "instance-current")
        .unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.kept, 1);
}

#[test]
fn lock_with_live_pid_and_forged_token_is_swept_by_pid_reuse_design() {
    // Documented boundary, not a bug being blessed: a live pid with a
    // lifetime token that does not match the process can only mean the
    // pid was recycled after its previous owner died. Recovery honors
    // that reading and reclaims the directory. The same shape is what a
    // forged lock (attacker rewrote the token) looks like — the two are
    // indistinguishable, which is why the root is a trust boundary:
    // whoever can write lock files there could have deleted the data
    // themselves. This test pins the designed behavior so any change
    // to it is a conscious decision.
    let root = TestDir::new("sec-lock-reuse");
    let squatter = root.path().join("instance-squatter");
    write_manual_lock(
        &squatter,
        &[
            &format!("pid={}", std::process::id()),
            "start=windows:passed-lifetime-of-somebody-else",
        ],
    );
    std::fs::write(squatter.join("data.bin"), b"old").unwrap();

    let manager = cachegirl::RecoveryManager::new(root.path());
    let report = manager
        .recover(cachegirl::RecoveryPolicy::OnStartup, "instance-current")
        .unwrap();
    assert_eq!(report.removed, 1, "pid-reuse reading: reclaimed");
    assert!(!squatter.exists());
}

// -- 11. error paths carry context and never panic ---------------------------

#[test]
fn error_displays_carry_context_and_never_panic() {
    // every error value we can produce renders a non-empty message with
    // the offending input in it; none of this may panic, none of it may
    // render an empty string
    let root = TestDir::new("sec-error-display");
    let manager = build_manager(root.path(), "instance-sec");
    let ns = manager.namespace("ns").unwrap();

    let mut rendered = Vec::new();

    rendered.push(ns.create_file("bad/name").unwrap_err().to_string());
    match manager.namespace("ns/../other") {
        Ok(_) => panic!("ns/../other must not be accepted"),
        Err(e) => rendered.push(e.to_string()),
    }
    rendered.push(
        FilesystemBackend::new(root.path().join("missing-root"), "i", SymlinkPolicy::Reject)
            .map(|_| ())
            .map_err(|e| e.to_string())
            .unwrap_err(),
    );

    let mut meta = ResourceMeta::new("x", "ns", ResourceKind::File);
    meta.metadata.insert("path".into(), "relative/story".into());
    rendered.push(format!("{:?}", meta)); // Debug must not panic either

    // directly constructed errors still render context
    rendered.push(AccessError::UnsafeName("bad/name".into()).to_string());
    rendered.push(
        AccessError::OutsideRoot {
            path: PathBuf::from("relative/story"),
            root: PathBuf::from("relative/root"),
        }
        .to_string(),
    );
    rendered.push(AccessError::SymlinkRejected(PathBuf::from("link")).to_string());
    rendered.push(BackendError::PathEscape(PathBuf::from("escaped")).to_string());
    rendered.push(
        BackendError::Rejected {
            backend: "filesystem".into(),
            reason: "already exists".into(),
        }
        .to_string(),
    );
    rendered.push(
        QuotaError::SizeExceeded {
            limit: 1,
            requested: 2,
        }
        .to_string(),
    );
    rendered.push(ConfigError::InvalidInstanceId("..".into()).to_string());

    for (index, message) in rendered.iter().enumerate() {
        assert!(!message.trim().is_empty(), "error #{index} renders empty");
    }

    // a couple of targeted context checks: the offending input shows up
    assert!(rendered[0].contains("bad/name"));
    assert!(rendered[4].contains("bad/name"));
    assert!(rendered[7].contains("escaped"));
    assert!(rendered[10].contains(".."));
    manager.shutdown().unwrap();
}
