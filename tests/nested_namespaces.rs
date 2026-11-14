//! Nested namespace semantics: hierarchy, isolation, recursive quotas
//! and recursive cleanup.

use std::sync::Arc;

mod common;

use cachegirl::{ConfigError, Error, TempManager};
use common::TestDir;

fn test_manager(root: &std::path::Path) -> TempManager {
    TempManager::builder()
        .root(root)
        .instance_id("nested-test")
        .build()
        .unwrap()
}

#[cfg(feature = "filesystem")]
#[test]
fn namespace_paths_create_the_whole_hierarchy_on_disk() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());

    let deep = manager.namespace("a/b/c").unwrap();
    assert_eq!(deep.name(), "a/b/c");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    assert!(root.join("nested-test/a").is_dir());
    assert!(root.join("nested-test/a/b").is_dir());
    assert!(root.join("nested-test/a/b/c").is_dir());

    manager.shutdown().unwrap();
}

#[test]
fn intermediate_levels_are_registered_with_the_manager() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());

    let deep = manager.namespace("a/b/c").unwrap();
    let mut names = manager.namespaces().unwrap();
    names.sort();
    assert_eq!(names, vec!["a", "a/b", "a/b/c"]);
    // and through the child API, not only through the manager
    let child = manager.namespace("x").unwrap().namespace("y").unwrap();
    assert_eq!(child.name(), "x/y");
    assert_eq!(child.depth(), 2);
    assert_eq!(child.parent(), Some("x"));
    assert!(Arc::ptr_eq(
        &manager.namespace("x/y").unwrap(),
        &manager.namespace("x").unwrap().namespace("y").unwrap()
    ));
    assert_eq!(deep.parent(), Some("a/b"));

    manager.shutdown().unwrap();
}

#[test]
fn bad_namespace_paths_are_rejected() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());

    for path in [
        "", "a//b",   // empty segment
        "/a",     // leading separator
        "a/",     // trailing separator
        "a/../b", // traversal is not a valid segment
        "..",     // reserved
        "a/./b",  // current-dir segment
        "a\\b",   // backslash is not a path separator in namespace names
        "a b",    // whitespace
        "a/b\\c", // mixed separators
        "a/..",   // trailing traversal
    ] {
        assert!(
            manager.namespace(path).is_err(),
            "path {path:?} must be rejected"
        );
    }
    // an over-deep hierarchy is rejected too
    let too_deep = (0..=cachegirl::namespace::MAX_NAMESPACE_DEPTH)
        .map(|_| "lvl")
        .collect::<Vec<_>>()
        .join("/");
    assert!(manager.namespace(&too_deep).is_err());
    // nothing partial may be registered
    assert_eq!(manager.namespaces().unwrap().len(), 0);

    manager.shutdown().unwrap();
}

#[test]
fn paths_at_the_depth_limit_are_accepted() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());

    let exact = (0..cachegirl::namespace::MAX_NAMESPACE_DEPTH)
        .map(|_| "lvl")
        .collect::<Vec<_>>()
        .join("/");
    let ns = manager.namespace(&exact).unwrap();
    assert_eq!(ns.depth(), cachegirl::namespace::MAX_NAMESPACE_DEPTH);
    let one_too_many = format!("{exact}/lvl");
    assert!(manager.namespace(&one_too_many).is_err());
    assert!(ns.namespace("lvl").is_err());

    manager.shutdown().unwrap();
}

#[test]
fn namespace_conflicts_resolve_to_one_object() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());

    let a = manager.namespace("parent").unwrap();
    let b = manager.namespace("parent/child").unwrap();
    let b1 = manager
        .namespace("parent")
        .unwrap()
        .namespace("child")
        .unwrap();
    let b2 = manager.namespace("parent/child").unwrap();
    assert!(Arc::ptr_eq(&b, &b1));
    assert!(Arc::ptr_eq(&b1, &b2));
    let a_again = manager.namespace("parent").unwrap();
    assert!(Arc::ptr_eq(&a, &a_again));
    // creating the parent after the child keeps both alive, no overwrite
    assert_eq!(b.parent(), Some("parent"));

    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn child_quota_is_independent_of_the_parent() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let child = manager.namespace("parent/child").unwrap();
    // parent unlimited, child capped at 8 bytes of tracked payload
    child.set_quota(cachegirl::NamespaceQuota::with_max_bytes(8));

    let _first = child.create_buffer(8).unwrap();
    let err = child.create_buffer(8).unwrap_err();
    assert!(matches!(
        err,
        Error::Quota(cachegirl::QuotaError::SizeExceeded { .. })
    ));
    // the parent's own quota is untouched by the child's usage
    manager
        .namespace("parent")
        .unwrap()
        .set_quota(cachegirl::NamespaceQuota::with_max_bytes(64));
    assert!(
        manager
            .namespace("parent")
            .unwrap()
            .create_buffer(32)
            .is_ok()
    );

    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn parent_quota_caps_the_whole_subtree_family() {
    let dir = TestDir::new("nested");
    // parent limited to 100 bytes; child unlimited, manager unlimited
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("nested-quota")
        .build()
        .unwrap();
    let parent = manager.namespace("parent").unwrap();
    parent.set_quota(cachegirl::NamespaceQuota::with_max_bytes(100));
    let child = manager.namespace("parent/child").unwrap();

    let _first = child.create_buffer(64).unwrap();
    // subtree total is 64; one more 64-byte buffer would push the
    // parent over its 100-byte quota even though the child allows it
    let err = child.create_buffer(64).unwrap_err();
    assert!(matches!(
        err,
        Error::Quota(cachegirl::QuotaError::SizeExceeded { .. })
    ));
    // and a 36-byte one fits exactly
    assert!(child.create_buffer(36).is_ok());

    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn grandchild_counts_against_every_ancestor() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let root = manager.namespace("root").unwrap();
    root.set_quota(cachegirl::NamespaceQuota::with_max_bytes(128));
    let mid = manager.namespace("root/mid").unwrap();
    mid.set_quota(cachegirl::NamespaceQuota::with_max_bytes(96));
    let leaf = manager.namespace("root/mid/leaf").unwrap();

    let _a = leaf.create_buffer(64).unwrap();
    let _b = leaf.create_buffer(32).unwrap(); // subtree: 96 — at the mid limit
    let err = leaf.create_buffer(32).unwrap_err(); // 96+32 exceeds both mid and root
    assert!(matches!(
        err,
        Error::Quota(cachegirl::QuotaError::SizeExceeded { .. })
    ));

    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn manager_quota_counts_nested_resources_too() {
    let dir = TestDir::new("nested");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("nested-manager-quota")
        .max_size(64)
        .build()
        .unwrap();
    let a = manager.namespace("x/y/z").unwrap();
    let _first = a.create_buffer(64).unwrap();
    let err = manager
        .namespace("x/w")
        .unwrap()
        .create_buffer(1)
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Quota(cachegirl::QuotaError::SizeExceeded { .. })
    ));

    manager.shutdown().unwrap();
}

#[cfg(feature = "filesystem")]
#[test]
fn resources_cannot_cross_namespaces() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let parent = manager.namespace("parent").unwrap();
    let child = manager.namespace("parent/child").unwrap();

    let file = parent.create_file("secret.bin").unwrap();
    let id = file.id().clone();
    // the child does not see it, cannot release it, and gets NotFound
    assert_eq!(child.resources().unwrap().len(), 0);
    assert!(child.resources().unwrap().is_empty());
    assert!(child.release(&id).is_err());
    // the parent can
    assert!(parent.release(&id).unwrap());

    manager.shutdown().unwrap();
}

#[cfg(feature = "filesystem")]
#[test]
fn recursive_cleanup_reclaims_descendants() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let child = manager.namespace("parent/child").unwrap();

    let file = child
        .create_file_with(
            "stale.bin",
            cachegirl::CreateOptions::new().ttl(core::time::Duration::from_secs(60)),
        )
        .unwrap();
    let path = file.path().to_path_buf();
    let id = file.id().clone();
    let stale = std::time::SystemTime::now() - core::time::Duration::from_secs(7200);
    manager
        .registry()
        .update(&id, |m| m.created_at = stale)
        .unwrap();

    // a pass on the parent reclaims the child's expired resource
    let report = manager
        .namespace("parent")
        .unwrap()
        .cleanup_expired()
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(!path.exists());

    manager.shutdown().unwrap();
}

#[cfg(feature = "filesystem")]
#[test]
fn child_cleanup_does_not_touch_the_parent() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let parent = manager.namespace("parent").unwrap();
    let child = manager.namespace("parent/child").unwrap();

    let p = parent
        .create_file_with(
            "parent-stale.bin",
            cachegirl::CreateOptions::new().ttl(core::time::Duration::from_secs(60)),
        )
        .unwrap();
    let c = child
        .create_file_with(
            "child-stale.bin",
            cachegirl::CreateOptions::new().ttl(core::time::Duration::from_secs(60)),
        )
        .unwrap();
    let stale = std::time::SystemTime::now() - core::time::Duration::from_secs(7200);
    for id in [p.id(), c.id()] {
        manager
            .registry()
            .update(id, |m| m.created_at = stale)
            .unwrap();
    }

    let report = child.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(
        manager.registry().get(&p.id().clone()).is_ok(),
        "parent's resource must survive the child's pass"
    );
    assert!(manager.registry().get(&c.id().clone()).is_err());

    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn usage_recursive_sums_the_subtree() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let parent = manager.namespace("parent").unwrap();
    let child = manager.namespace("parent/child").unwrap();
    let grandchild = manager.namespace("parent/child/leaf").unwrap();

    let _p = parent.create_buffer(10).unwrap();
    let _c = child.create_buffer(20).unwrap();
    let _g = grandchild.create_buffer(40).unwrap();

    assert_eq!(parent.usage().unwrap(), (10, 1));
    assert_eq!(child.usage().unwrap(), (20, 1));
    assert_eq!(parent.usage_recursive().unwrap(), (70, 3));
    assert_eq!(child.usage_recursive().unwrap(), (60, 2));
    assert_eq!(grandchild.usage_recursive().unwrap(), (40, 1));

    manager.shutdown().unwrap();
}

#[test]
fn path_validation_reports_the_full_path() {
    let dir = TestDir::new("nested");
    let manager = test_manager(dir.path());
    let err = match manager.namespace("ok/../../escape") {
        Ok(_) => panic!("expected the path to be rejected"),
        Err(err) => err,
    };
    match err {
        Error::Config(ConfigError::InvalidNamespaceName(echo)) => {
            assert_eq!(echo, "ok/../../escape")
        }
        other => panic!("expected InvalidNamespaceName, got {other:?}"),
    }

    manager.shutdown().unwrap();
}
