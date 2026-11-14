//! Shutdown contract: lease waiting, instance directory removal, confinement.

#![cfg(feature = "filesystem")]

use std::time::{Duration, Instant};

mod common;

use cachegirl::TempManager;
use common::{TestDir, backdate};

fn manager_for(dir: &TestDir, instance: &str) -> TempManager {
    TempManager::builder()
        .root(dir.path())
        .instance_id(instance)
        .build()
        .unwrap()
}

#[test]
fn shutdown_keeps_the_instance_dir_by_default() {
    let dir = TestDir::new("shutdown-default");
    let manager = manager_for(&dir, "default-instance");
    let ns = manager.namespace("keep").unwrap();
    let file = ns.create_file("data.bin").unwrap();
    let path = file.path().to_path_buf();
    assert!(path.exists());
    manager.shutdown().unwrap();
    // nothing is removed by default: the dir and the file stay
    assert!(dir.path().join("default-instance").exists());
    assert!(path.exists());
}

#[test]
fn shutdown_removes_the_instance_dir_when_enabled() {
    let dir = TestDir::new("shutdown-remove");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("doomed-instance")
        .remove_instance_dir_on_shutdown(true)
        .build()
        .unwrap();
    let ns = manager.namespace("gone").unwrap();
    let file = ns.create_file("data.bin").unwrap();
    let path = file.path().to_path_buf();
    assert!(path.exists());

    manager.shutdown().unwrap();
    // the whole instance dir is gone, the root stays
    assert!(!dir.path().join("doomed-instance").exists());
    assert!(dir.path().exists());
}

#[test]
fn shutdown_removal_leaves_sibling_instances_untouched() {
    let dir = TestDir::new("shutdown-siblings");
    let sibling = dir.path().join("sibling-instance");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("precious.bin"), b"do not delete").unwrap();

    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("doomed-instance")
        .remove_instance_dir_on_shutdown(true)
        .build()
        .unwrap();
    let ns = manager.namespace("gone").unwrap();
    ns.create_file("data.bin").unwrap();
    manager.shutdown().unwrap();

    assert!(!dir.path().join("doomed-instance").exists());
    assert!(sibling.join("precious.bin").exists());
}

#[test]
fn shutdown_removal_confined_to_the_root() {
    let dir = TestDir::new("shutdown-confine");
    let outside = TestDir::new("shutdown-confine-outside");
    std::fs::write(outside.path().join("victim.bin"), b"precious").unwrap();

    // plant the instance path as a symlink pointing outside the root;
    // shutdown must refuse and delete nothing
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("linked-instance")
        .remove_instance_dir_on_shutdown(true)
        .build()
        .unwrap();
    let instance = dir.path().join("linked-instance");
    std::fs::remove_dir_all(&instance).unwrap();
    if !plant_symlink_dir(outside.path(), &instance) {
        // platform cannot plant symlinks here; nothing to test
        manager.shutdown().unwrap();
        return;
    }
    let err = manager.shutdown().unwrap_err();
    assert!(
        matches!(
            err,
            cachegirl::Error::Access(cachegirl::AccessError::OutsideRoot { .. })
        ),
        "expected OutsideRoot, got {err:?}"
    );
    assert!(outside.path().join("victim.bin").exists());
    // the link itself is untouched; the manager did not delete anything
    assert!(std::fs::symlink_metadata(&instance).is_ok());
}

#[test]
fn shutdown_removal_refuses_a_link_to_the_root_itself() {
    let dir = TestDir::new("shutdown-confine-root");
    std::fs::write(dir.path().join("precious.bin"), b"keep me").unwrap();

    // the instance path is a symlink pointing at the root itself;
    // deleting it would take the root and everything above out with it
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("linked-instance")
        .remove_instance_dir_on_shutdown(true)
        .build()
        .unwrap();
    let instance = dir.path().join("linked-instance");
    std::fs::remove_dir_all(&instance).unwrap();
    if !plant_symlink_dir(dir.path(), &instance) {
        manager.shutdown().unwrap();
        return;
    }
    let err = manager.shutdown().unwrap_err();
    assert!(
        matches!(
            err,
            cachegirl::Error::Access(cachegirl::AccessError::OutsideRoot { .. })
        ),
        "expected OutsideRoot, got {err:?}"
    );
    assert!(dir.path().join("precious.bin").exists());
}

#[cfg(unix)]
fn plant_symlink_dir(target: &std::path::Path, link: &std::path::Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn plant_symlink_dir(target: &std::path::Path, link: &std::path::Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

#[test]
fn shutdown_without_lease_timeout_defers_everything() {
    let dir = TestDir::new("shutdown-nodefer");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("nodefer")
        .cleanup_on_shutdown(true)
        .build()
        .unwrap();
    let ns = manager.namespace("leased").unwrap();
    let file = ns
        .create_file_with(
            "held.bin",
            cachegirl::CreateOptions::new().ttl(Duration::from_secs(60)),
        )
        .unwrap();
    let id = file.id().clone();
    let path = file.path().to_path_buf();
    let _lease = file.acquire().unwrap();
    backdate(&manager, &id, 7200);

    let started = Instant::now();
    let report = manager.shutdown().unwrap();
    // lease is never dropped, so the resource cannot be reclaimed
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(report.deferred, 1);
    assert!(path.exists());
    let meta = manager.registry().get(&id).unwrap();
    assert_eq!(meta.state, cachegirl::ResourceState::PendingCleanup);
}

#[test]
fn shutdown_waits_for_leases_and_then_sweeps() {
    let dir = TestDir::new("shutdown-wait");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("waits")
        .cleanup_on_shutdown(true)
        .shutdown_wait_leases(Duration::from_secs(10))
        .build()
        .unwrap();
    let ns = manager.namespace("leased").unwrap();
    let file = ns
        .create_file_with(
            "held.bin",
            cachegirl::CreateOptions::new().ttl(Duration::from_secs(60)),
        )
        .unwrap();
    let id = file.id().clone();
    let path = file.path().to_path_buf();
    backdate(&manager, &id, 7200);
    // an in-flight operation holds the lease; when it finishes it drops it
    let lease = file.acquire().unwrap();

    let worker = std::thread::spawn(move || {
        // stand-in for a long-running operation; it releases its lease
        // shortly before the wait budget would expire
        std::thread::sleep(Duration::from_millis(150));
        drop(lease);
        drop(file);
    });

    let started = Instant::now();
    let report = manager.shutdown().unwrap();
    let took = started.elapsed();
    worker.join().unwrap();

    // the wait must have actually waited for the lease
    assert!(
        took >= Duration::from_millis(150),
        "shutdown did not wait: {took:?}"
    );
    assert_eq!(report.removed, 1);
    assert!(!path.exists());
    assert_eq!(manager.stats().unwrap().active_leases, 0);
}

#[test]
fn shutdown_wait_timeout_reports_deferred() {
    let dir = TestDir::new("shutdown-timeout");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("timeout")
        .cleanup_on_shutdown(true)
        .shutdown_wait_leases(Duration::from_millis(250))
        .build()
        .unwrap();
    let ns = manager.namespace("leased").unwrap();
    let file = ns
        .create_file_with(
            "held.bin",
            cachegirl::CreateOptions::new().ttl(Duration::from_secs(60)),
        )
        .unwrap();
    let id = file.id().clone();
    let path = file.path().to_path_buf();
    let lease = file.acquire().unwrap();
    backdate(&manager, &id, 7200);

    let started = Instant::now();
    let report = manager.shutdown().unwrap();
    let took = started.elapsed();
    // never returns instantly, and the record stays pending
    assert!(
        took >= Duration::from_millis(200),
        "wait returned too early: {took:?}"
    );
    assert_eq!(report.deferred, 1);
    assert!(path.exists());
    drop(lease);
    let meta = manager.registry().get(&id).unwrap();
    assert_eq!(meta.state, cachegirl::ResourceState::PendingCleanup);
}

#[test]
fn shutdown_wait_pools_stats_and_still_blocks_operations() {
    let dir = TestDir::new("shutdown-wait-blocks");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("blocks")
        .shutdown_wait_leases(Duration::from_secs(10))
        .build()
        .unwrap();
    let ns = manager.namespace("nope").unwrap();
    let file = ns.create_file("held.bin").unwrap();
    let lease = file.acquire().unwrap();

    let started = Instant::now();
    std::thread::spawn(move || drop(lease)).join().unwrap();
    let report = manager.shutdown().unwrap();
    let took = started.elapsed();
    // shutdown returns once the lease is gone (the spawn dropped it already)
    assert!(took < Duration::from_secs(10));
    assert_eq!(report.scanned, 0); // cleanup_on_shutdown is not set
    // but creation is blocked now, leases counted at zero anyway
    let stats = manager.stats().unwrap();
    assert_eq!(stats.active_leases, 0);
    assert!(ns.create_file("after.bin").is_err());
}

#[test]
fn second_shutdown_call_is_a_no_op() {
    let dir = TestDir::new("shutdown-twice");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("twice")
        .cleanup_on_shutdown(true)
        .remove_instance_dir_on_shutdown(true)
        .build()
        .unwrap();
    let ns = manager.namespace("once").unwrap();
    ns.create_file("data.bin").unwrap();

    let first = manager.shutdown().unwrap();
    let second = manager.shutdown().unwrap();
    assert_eq!(second.scanned, 0);
    assert_eq!(second.removed, 0);
    assert!(!dir.path().join("twice").exists());
    // first report was a real pass over zero expired resources
    assert_eq!(first.scanned, 1);
}
