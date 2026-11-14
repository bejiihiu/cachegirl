//! End-to-end quota enforcement and ownership semantics.
//!
//! Byte and count limits at both scopes (manager and namespace),
//! typed rejection errors, and the drain-and-create-again cycle
//! after expired resources are reclaimed. Then the two ownership
//! modes whose destruction is driven by handles rather than by
//! release calls.

#![cfg(all(feature = "filesystem", feature = "memory"))]

mod common;

use std::time::Duration;

use cachegirl::{CreateOptions, NamespaceQuota, Ownership, QuotaError, ResourceState};
use common::{TestDir, backdate};

#[test]
fn manager_and_namespace_quotas_reject_then_release_capacity() {
    let dir = TestDir::new("quotas");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-quotas")
        .max_size(1024) // manager-wide byte cap
        .build()
        .unwrap();

    let limited = manager.namespace("limited").unwrap();
    limited.set_quota(NamespaceQuota::with_limits(512, 2));

    // fill the namespace to the byte edge: 512 of 512 used
    let big = limited
        .create_buffer_with(512, CreateOptions::new().ttl(Duration::from_secs(3600)))
        .unwrap();

    // one more byte anywhere in this namespace must be refused, typed
    let err = limited.create_buffer(1).unwrap_err();
    assert!(matches!(
        err,
        cachegirl::Error::Quota(QuotaError::SizeExceeded { limit: 512, .. })
    ));

    // zero-byte buffers still fit byte-wise; the count limit catches those
    let small = limited
        .create_buffer_with(0, CreateOptions::new().ttl(Duration::from_secs(3600)))
        .unwrap();
    let err = limited.create_buffer(0).unwrap_err();
    assert!(matches!(
        err,
        cachegirl::Error::Quota(QuotaError::CountExceeded { limit: 2 })
    ));

    // a different namespace is not bound by `limited`'s quota, but the
    // manager-wide byte cap still applies to everything it creates
    let other = manager.namespace("other").unwrap();
    let pad = other
        .create_buffer_with(512, CreateOptions::new().ttl(Duration::from_secs(3600)))
        .unwrap(); // 512 + 512 = 1024 of 1024
    let err = other.create_buffer(1).unwrap_err();
    assert!(matches!(
        err,
        cachegirl::Error::Quota(QuotaError::SizeExceeded { limit: 1024, .. })
    ));

    // nothing was registered by the rejected creations
    assert_eq!(manager.registry().len().unwrap(), 3);

    // once the expired tenants are swept, capacity is available again
    backdate(&manager, big.id(), 7200);
    backdate(&manager, small.id(), 7200);
    backdate(&manager, pad.id(), 7200);
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 3);
    assert_eq!(limited.usage().unwrap(), (0, 0));

    let retry = limited.create_buffer(512).unwrap();
    assert_eq!(retry.capacity(), 512);
    manager.shutdown().unwrap();
}

#[test]
fn shared_ownership_last_drop_destroys_the_resource() {
    let dir = TestDir::new("shared");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-shared")
        .build()
        .unwrap();
    let ns = manager.namespace("pair").unwrap();

    // component A creates a shared resource; component B gets a co-owner handle
    let first = ns
        .create_file_with(
            "shared.bin",
            CreateOptions::new().ownership(Ownership::Shared),
        )
        .unwrap();
    std::fs::write(first.path(), b"shared payload").unwrap();
    let path = first.path().to_path_buf();
    let id = first.id().clone();
    let second = first.clone();

    // component B does its work in its own thread and drops out first
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let data = std::fs::read(second.path()).unwrap();
            assert_eq!(data, b"shared payload");
            // `second` drops here — `first` keeps the resource alive
        });
    });

    // one co-owner gone, the file and its record both survive
    assert!(path.exists());
    assert!(manager.registry().get(&id).is_ok());

    // the last component drops out: the resource is really destroyed
    drop(first);
    assert!(!path.exists());
    assert!(manager.registry().get(&id).is_err());
    manager.shutdown().unwrap();
}

#[test]
fn release_during_lease_defers_and_the_sweeper_collects() {
    let dir = TestDir::new("leased");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-leased")
        .build()
        .unwrap();
    let ns = manager.namespace("jobs").unwrap();

    let file = ns
        .create_file_with(
            "held.bin",
            CreateOptions::new().ownership(Ownership::Leased),
        )
        .unwrap();
    let path = file.path().to_path_buf();
    let id = file.id().clone();

    // hold a lease, ask for release: it must be parked, not deleted
    let lease = file.acquire().unwrap();
    assert!(!ns.release(&id).unwrap());
    assert!(path.exists());
    assert_eq!(
        manager.registry().get(&id).unwrap().state,
        ResourceState::PendingCleanup
    );

    // drop the lease; the next sweep is what actually destroys the file
    drop(lease);
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.failed, 0);
    assert!(!path.exists());
    assert!(manager.registry().get(&id).is_err());
    manager.shutdown().unwrap();
}
