//! End-to-end integration of user-registered custom backends.
//!
//! The backend below is a real implementation of the `ResourceBackend`
//! contract — it counts calls, records what it was asked to do, and can
//! be told to fail on create or on release. Nothing is stubbed out:
//! the manager talks to it through the same path every built-in
//! backend uses.

#![cfg(feature = "filesystem")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use cachegirl::backend::{ResourceBackend, ResourceRequest};
use cachegirl::{
    BackendError, CreateOptions, Error, Event, ResourceId, ResourceKind, ResourceMeta,
    ResourceState, VecSink,
};
use common::{TestDir, backdate};

/// A backend that logs everything it does. Failure switches flip on
/// demand, which is how the rollback paths get exercised for real.
struct LoggingBackend {
    creates: AtomicUsize,
    releases: AtomicUsize,
    fail_create: AtomicBool,
    fail_release: AtomicBool,
    requests: std::sync::Mutex<Vec<ResourceRequest>>,
    released_ids: std::sync::Mutex<Vec<ResourceId>>,
}

impl LoggingBackend {
    fn new() -> Self {
        Self {
            creates: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            fail_create: AtomicBool::new(false),
            fail_release: AtomicBool::new(false),
            requests: std::sync::Mutex::new(Vec::new()),
            released_ids: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ResourceBackend for LoggingBackend {
    fn name(&self) -> &str {
        "logging"
    }

    fn create(&self, request: &ResourceRequest) -> cachegirl::Result<ResourceMeta> {
        if self.fail_create.load(Ordering::SeqCst) {
            return Err(BackendError::Rejected {
                backend: self.name().into(),
                reason: "backend is configured to reject creation".into(),
            }
            .into());
        }
        self.creates.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let mut meta = ResourceMeta::new(&request.name, &request.namespace, request.kind);
        meta.size_bytes = request.size_hint.unwrap_or(0);
        Ok(meta)
    }

    fn release(&self, meta: &ResourceMeta) -> cachegirl::Result<()> {
        if self.fail_release.load(Ordering::SeqCst) {
            return Err(BackendError::Rejected {
                backend: self.name().into(),
                reason: "backend is configured to fail release".into(),
            }
            .into());
        }
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.released_ids.lock().unwrap().push(meta.id.clone());
        Ok(())
    }
}

#[test]
fn custom_backend_sees_create_and_release_end_to_end() {
    let dir = TestDir::new("custom-roundtrip");
    let sink = Arc::new(VecSink::new());
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-custom")
        .event_sink(sink.clone())
        .build()
        .unwrap();
    let backend = Arc::new(LoggingBackend::new());
    manager
        .register_backend(Arc::clone(&backend) as Arc<dyn ResourceBackend>)
        .unwrap();

    let ns = manager.namespace("jobs").unwrap();
    let resource = ns
        .create_custom_with(
            "job-handle",
            "logging",
            CreateOptions::new().metadata("purpose", "batch-import"),
        )
        .unwrap();

    // the backend really materialized the request
    assert_eq!(backend.creates.load(Ordering::SeqCst), 1);
    assert_eq!(backend.releases.load(Ordering::SeqCst), 0);
    let seen = backend.requests.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, "job-handle");
    assert_eq!(seen[0].namespace, "jobs");
    assert_eq!(seen[0].kind, ResourceKind::Custom);
    assert_eq!(
        seen[0].metadata.get("purpose").map(String::as_str),
        Some("batch-import")
    );
    drop(seen);

    assert_eq!(resource.backend(), "logging");
    assert!(manager.registry().get(resource.id()).is_ok());

    // release routes back into the very same backend
    let id = resource.id().clone();
    assert!(ns.release(&id).unwrap());
    assert_eq!(backend.releases.load(Ordering::SeqCst), 1);
    assert_eq!(*backend.released_ids.lock().unwrap(), vec![id.clone()]);
    assert!(manager.registry().get(&id).is_err());

    let events = sink.drain();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ResourceCreated {
            kind: ResourceKind::Custom,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ResourceReleased { .. }))
    );
    manager.shutdown().unwrap();
}

#[test]
fn backend_failing_on_create_propagates_and_leaves_the_registry_clean() {
    let dir = TestDir::new("custom-fail-create");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-fail-create")
        .build()
        .unwrap();
    let backend = Arc::new(LoggingBackend::new());
    backend.fail_create.store(true, Ordering::SeqCst);
    manager
        .register_backend(Arc::clone(&backend) as Arc<dyn ResourceBackend>)
        .unwrap();

    let ns = manager.namespace("jobs").unwrap();
    let err = ns.create_custom("doomed-handle", "logging").unwrap_err();
    assert!(matches!(
        err,
        Error::Backend(BackendError::Rejected { ref reason, .. }) if reason.contains("reject")
    ));

    // nothing leaked into the registry, and the backend never claimed success
    assert_eq!(manager.registry().len().unwrap(), 0);
    assert_eq!(backend.creates.load(Ordering::SeqCst), 0);

    // the failure is not sticky: once the backend heals, creation works
    backend.fail_create.store(false, Ordering::SeqCst);
    let healthy = ns.create_custom("healthy-handle", "logging").unwrap();
    assert!(manager.registry().get(healthy.id()).is_ok());
    manager.shutdown().unwrap();
}

#[test]
fn backend_failing_on_release_marks_failed_cleanup_and_the_sweep_retries() {
    let dir = TestDir::new("custom-fail-release");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-fail-release")
        .build()
        .unwrap();
    let backend = Arc::new(LoggingBackend::new());
    manager
        .register_backend(Arc::clone(&backend) as Arc<dyn ResourceBackend>)
        .unwrap();

    let ns = manager.namespace("jobs").unwrap();
    let resource = ns.create_custom("fragile-handle", "logging").unwrap();
    let id = resource.id().clone();

    // release blows up: error is visible, record is kept and marked
    backend.fail_release.store(true, Ordering::SeqCst);
    let err = ns.release(&id).unwrap_err();
    assert!(matches!(err, Error::Backend(BackendError::Rejected { .. })));
    let meta = manager.registry().get(&id).unwrap();
    assert_eq!(meta.state, ResourceState::FailedCleanup);
    assert_eq!(backend.releases.load(Ordering::SeqCst), 0);

    // a cleanup pass absorbs the failure instead of going down with it
    let first = manager.cleanup_expired().unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.failure_reasons.len(), 1);
    assert!(manager.registry().get(&id).is_ok());

    // heal the backend: the sweeper retries the marked record and wins
    backend.fail_release.store(false, Ordering::SeqCst);
    let second = manager.cleanup_expired().unwrap();
    assert_eq!(second.removed, 1);
    assert_eq!(backend.releases.load(Ordering::SeqCst), 1);
    assert_eq!(*backend.released_ids.lock().unwrap(), vec![id.clone()]);
    assert!(manager.registry().get(&id).is_err());
    manager.shutdown().unwrap();
}

#[test]
fn application_driven_scheduler_loop_reclaims_expired_resources() {
    let dir = TestDir::new("scheduler");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-scheduler")
        .build()
        .unwrap();
    let ns = manager.namespace("work").unwrap();
    let file = ns
        .create_file_with(
            "scratch.bin",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    std::fs::write(file.path(), b"payload").unwrap();
    let file_id = file.id().clone();
    let path = file.path().to_path_buf();
    backdate(&manager, &file_id, 7200);

    // the library never spawns threads; the application runs this loop
    // wherever it wants (a timer, a work queue, a shutdown hook)
    let reclaimed = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let mut total = 0u64;
            let mut ticks = 0u32;
            while total == 0 && ticks < 200 {
                total += manager.cleanup_expired().unwrap().removed;
                ticks += 1;
                if total == 0 {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            total
        });
        handle.join().unwrap()
    });

    assert_eq!(reclaimed, 1);
    assert!(!path.exists());
    assert!(manager.registry().get(&file_id).is_err());
    manager.shutdown().unwrap();
}
