//! End-to-end lifecycle: from manager construction to reclamation.
//!
//! These tests drive the public API exactly the way an application
//! would — build a manager, carve out namespaces, create resources,
//! use them, let their TTLs run out, and reclaim them — and assert on
//! what the outside world sees: files on disk, the registry, stats and
//! the event stream.

#![cfg(feature = "filesystem")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use cachegirl::{CreateOptions, Event, Ownership, VecSink};
use common::{TestDir, backdate};

#[test]
fn full_lifecycle_from_creation_to_reclamation() {
    let dir = TestDir::new("lifecycle");
    let sink = Arc::new(VecSink::new());
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-lifecycle")
        .cleanup_on_start(true) // fresh root: nothing to reclaim, but a report must exist
        .cleanup_on_shutdown(true)
        .event_sink(sink.clone())
        .build()
        .unwrap();

    let recovery = manager
        .recovery_report()
        .expect("cleanup_on_start must produce a report");
    assert_eq!(recovery.removed, 0);

    let alpha = manager.namespace("alpha").unwrap();
    let beta = manager.namespace("beta").unwrap();

    // one resource per namespace, different kinds and fates
    let payload = b"hello cachegirl";
    let file = alpha
        .create_file_with(
            "packet.bin",
            CreateOptions::new()
                .ttl(Duration::from_secs(3600))
                .owner("net-stack")
                .metadata("purpose", "test-packet"),
        )
        .unwrap();
    std::fs::write(file.path(), payload).unwrap();
    alpha.refresh_size(file.id()).unwrap();

    let stale_dir = alpha
        .create_directory_with(
            "scratch",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    let work_dir = beta.create_directory("work").unwrap();

    let file_id = file.id().clone();
    let stale_dir_id = stale_dir.id().clone();
    let work_dir_id = work_dir.id().clone();

    // -- usage and stats before anything expires --
    let (bytes, count) = alpha.usage().unwrap();
    assert_eq!(count, 2);
    assert!(bytes >= payload.len() as u64);

    let before = manager.stats().unwrap();
    assert_eq!(before.resources, 3);
    assert_eq!(before.namespaces, 2);
    assert_eq!(before.gc_runs, 0);
    assert!(before.bytes_tracked >= payload.len() as u64);

    // -- TTLs run out, without sleeping --
    backdate(&manager, &file_id, 7200);
    backdate(&manager, &stale_dir_id, 7200);

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(report.reasons.ttl_fixed, 2);

    // physical side is really gone, registry agrees
    assert!(!file.path().exists());
    assert!(!stale_dir.path().exists());
    assert!(manager.registry().get(&file_id).is_err());
    assert!(manager.registry().get(&stale_dir_id).is_err());
    assert!(manager.registry().get(&work_dir_id).is_ok());

    let after = manager.stats().unwrap();
    assert_eq!(after.resources, 1);
    assert_eq!(after.gc_runs, 1);
    // destroyed resources are counted exactly once, by the destroy path
    assert_eq!(after.resources_released, 2);
    assert_eq!(
        after.bytes_freed,
        before.bytes_tracked - after.bytes_tracked
    );
    assert!(after.bytes_freed >= payload.len() as u64);
    assert_eq!(after.pending_cleanup, 0);

    // -- the event stream tells the same story --
    let events = sink.drain();
    let created = events
        .iter()
        .filter(|e| matches!(e, Event::ResourceCreated { .. }))
        .count();
    let released = events
        .iter()
        .filter(|e| matches!(e, Event::ResourceReleased { .. }))
        .count();
    let cleanups = events
        .iter()
        .filter(|e| matches!(e, Event::CleanupCompleted { .. }))
        .count();
    assert_eq!(created, 3);
    assert_eq!(released, 2);
    assert_eq!(cleanups, 1);
    let removed_reported = events
        .iter()
        .find_map(|e| match e {
            Event::CleanupCompleted { removed, .. } => Some(*removed),
            _ => None,
        })
        .unwrap();
    assert_eq!(removed_reported, 2);

    // -- graceful shutdown sweeps nothing: nothing is overdue anymore --
    let shutdown_report = manager.shutdown().unwrap();
    assert_eq!(shutdown_report.removed, 0);
    assert!(
        work_dir.path().exists(),
        "an in-ttl directory must survive shutdown"
    );
    assert!(
        !dir.path()
            .join("instance-lifecycle")
            .join(".cachegirl.lock")
            .exists(),
        "a clean shutdown must remove the instance lock"
    );
}

#[test]
fn shutdown_sweeps_expired_and_pending_then_stays_safe() {
    let dir = TestDir::new("shutdown");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-shutdown")
        .cleanup_on_shutdown(true)
        .build()
        .unwrap();
    let ns = manager.namespace("work").unwrap();

    let expired_file = ns
        .create_file_with(
            "stale.bin",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    let expired_dir = ns
        .create_directory_with(
            "stale-dir",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    let pending = ns
        .create_file_with(
            "held.bin",
            CreateOptions::new().ownership(Ownership::Leased),
        )
        .unwrap();
    let fresh = ns
        .create_file_with(
            "fresh.bin",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();

    // release while a lease is held: the resource must be parked, not deleted
    let lease = pending.acquire().unwrap();
    assert!(!ns.release(pending.id()).unwrap());
    drop(lease); // sweeper's turn now

    backdate(&manager, expired_file.id(), 7200);
    backdate(&manager, expired_dir.id(), 7200);
    // `fresh` stays young and must ride out the sweep

    let report = manager.shutdown().unwrap();
    assert_eq!(report.removed, 3);
    assert_eq!(report.failed, 0);
    assert!(!expired_file.path().exists());
    assert!(!expired_dir.path().exists());
    assert!(!pending.path().exists());
    assert!(fresh.path().exists());

    // lock is gone, and calling shutdown again is a harmless no-op
    assert!(
        !dir.path()
            .join("instance-shutdown")
            .join(".cachegirl.lock")
            .exists()
    );
    let again = manager.shutdown().unwrap();
    assert_eq!(again.removed, 0);
    // the sweep already ran on the first call; the repeat is a pure no-op
    assert_eq!(again.scanned, 0);
}
