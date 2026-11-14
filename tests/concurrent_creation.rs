//! Concurrency guarantees of resource creation.
//!
//! The creation path used to serialize through one global mutex, which
//! made unrelated namespaces wait on each other. Quota enforcement is
//! now a registry-atomic reservation instead: namespaces without shared
//! quota progress fully in parallel, and namespaces with shared quota
//! cannot overshoot.

#![cfg(feature = "filesystem")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

mod common;

use cachegirl::backend::{ResourceBackend, ResourceRequest};
use cachegirl::{Error, ResourceMeta, TempManager};
use common::TestDir;

/// A backend that blocks inside `create` until told to release. Used
/// to prove that a slow creation in one namespace does not block a
/// creation in another.
struct GateBackend {
    entered: AtomicBool,
    release: Mutex<bool>,
    cv: Condvar,
}

impl GateBackend {
    fn open() -> Self {
        Self {
            entered: AtomicBool::new(false),
            release: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn was_entered(&self) -> bool {
        self.entered.load(Ordering::SeqCst)
    }

    fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

impl ResourceBackend for GateBackend {
    fn name(&self) -> &str {
        "gate"
    }

    fn create(&self, request: &ResourceRequest) -> cachegirl::Result<ResourceMeta> {
        self.entered.store(true, Ordering::SeqCst);
        let mut released = self.release.lock().unwrap();
        while !*released {
            released = self.cv.wait(released).unwrap();
        }
        Ok(ResourceMeta::new(
            &request.name,
            &request.namespace,
            request.kind,
        ))
    }

    fn release(&self, _meta: &ResourceMeta) -> cachegirl::Result<()> {
        Ok(())
    }
}

#[test]
fn creates_in_different_namespaces_do_not_serialize() {
    let dir = TestDir::new("parallel-namespaces");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("parallel")
        .build()
        .unwrap();
    let gate = Arc::new(GateBackend::open());
    manager.register_backend(gate.clone()).unwrap();

    let ns1 = Arc::new(manager.namespace("one").unwrap());
    let ns1_for_thread = Arc::clone(&ns1);
    let handle = std::thread::spawn(move || ns1_for_thread.create_custom("widget", "gate"));

    // wait until the gated creation is actually inside the backend
    let start = std::time::Instant::now();
    while !gate.was_entered() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "gated creation never reached the backend"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // an unrelated namespace must sail through while ns1 is blocked
    let ns2 = manager.namespace("two").unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = ns2.create_file("unblocked.bin").map(|_| ());
        let _ = done_tx.send(result);
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("creation in an unrelated namespace was blocked (serialization regression)")
        .expect("creation in an unrelated namespace failed");

    gate.release();
    handle.join().unwrap().unwrap();
    manager.shutdown().unwrap();
}

#[test]
fn concurrent_creates_in_one_namespace_obey_the_count_quota() {
    let dir = TestDir::new("parallel-exact");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("exact")
        .build()
        .unwrap();
    let ns = Arc::new(manager.namespace("limited").unwrap());
    // exactly ten slots; far more attempts than that
    ns.set_quota(cachegirl::NamespaceQuota::with_max_resources(10));

    let mut handles = Vec::new();
    for worker in 0..8 {
        let ns = Arc::clone(&ns);
        handles.push(std::thread::spawn(move || {
            let mut ok = 0u32;
            for n in 0..4 {
                match ns.create_file(&format!("worker-{worker}-{n}.bin")) {
                    Ok(_) => ok += 1,
                    Err(Error::Quota(cachegirl::QuotaError::CountExceeded { .. })) => {}
                    Err(other) => panic!("unexpected error: {other}"),
                }
            }
            ok
        }));
    }
    let total_ok: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total_ok, 10);
    assert_eq!(manager.registry().len().unwrap(), 10);
    manager.shutdown().unwrap();
}

#[test]
fn concurrent_creates_across_namespaces_obey_the_manager_quota() {
    let dir = TestDir::new("parallel-manager-quota");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("manager-quota")
        .max_resources(10)
        .build()
        .unwrap();

    let mut handles = Vec::new();
    for ns_idx in 0..6 {
        for worker in 0..4 {
            let ns = Arc::new(manager.namespace(&format!("ns-{ns_idx}")).unwrap());
            handles.push(std::thread::spawn(move || {
                let mut ok = 0u32;
                for n in 0..4 {
                    match ns.create_file(&format!("w-{worker}-{n}.bin")) {
                        Ok(_) => ok += 1,
                        Err(Error::Quota(cachegirl::QuotaError::CountExceeded { .. })) => {}
                        Err(other) => panic!("unexpected error: {other}"),
                    }
                }
                ok
            }));
        }
    }
    let total_ok: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total_ok, 10);
    assert_eq!(manager.registry().len().unwrap(), 10);
    manager.shutdown().unwrap();
}

#[cfg(feature = "memory")]
#[test]
fn concurrent_buffer_creations_under_a_size_quota_never_overshoot() {
    let dir = TestDir::new("parallel-size-quota");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("size-quota")
        .build()
        .unwrap();
    let ns = Arc::new(manager.namespace("limited").unwrap());
    ns.set_quota(cachegirl::NamespaceQuota::with_max_bytes(1280));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let ns = Arc::clone(&ns);
        handles.push(std::thread::spawn(move || {
            let mut ok = 0u32;
            for _ in 0..8 {
                match ns.create_buffer(256) {
                    Ok(_) => ok += 1,
                    Err(Error::Quota(cachegirl::QuotaError::SizeExceeded { .. })) => {}
                    Err(other) => panic!("unexpected error: {other}"),
                }
            }
            ok
        }));
    }
    let total_ok: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total_ok, 5);
    let (bytes, count) = ns.usage().unwrap();
    assert_eq!(count, 5);
    assert_eq!(bytes, 1280);
    manager.shutdown().unwrap();
}

#[test]
fn quota_counters_agree_with_the_registry_under_contention() {
    let dir = TestDir::new("parallel-consistency");
    let manager = TempManager::builder()
        .root(dir.path())
        .instance_id("consistency")
        .build()
        .unwrap();
    let ns1 = Arc::new(manager.namespace("consistency/a").unwrap());
    let ns2 = Arc::new(manager.namespace("consistency/b").unwrap());

    let mut handles = Vec::new();
    for ns in [ns1, ns2] {
        handles.push(std::thread::spawn(move || {
            for n in 0..20 {
                let _ = ns.create_file(&format!("f-{n}.bin"));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let a = manager.namespace("consistency/a").unwrap().usage().unwrap();
    let b = manager.namespace("consistency/b").unwrap().usage().unwrap();
    let parent = manager.namespace("consistency").unwrap();
    let recursive = parent.usage_recursive().unwrap();
    assert_eq!(recursive.0, a.0 + b.0);
    assert_eq!(recursive.1, a.1 + b.1);
    assert_eq!(recursive.1, 40);

    manager.shutdown().unwrap();
}
