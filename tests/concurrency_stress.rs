//! Concurrency stress: the failure modes that only surface under load
//! or unlucky interleaving, forced into a few seconds of test time.
//!
//! Thread counts and iteration budgets are pinned constants, tuned to
//! keep the suite fast and stable on ordinary hardware. Correctness
//! never depends on sleeps: coordination goes through barriers and
//! atomics, and loop-scoping does the rest (the scenario repeats, and
//! the invariant is asserted every iteration). The final consistency
//! check in every scenario is two-way: the registry and the on-disk
//! layout must agree exactly.
//!
//! What each scenario is really hunting, and why it is wired up the way
//! it is, is documented on the test itself.

#![cfg(all(feature = "filesystem", feature = "memory", feature = "cache"))]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use cachegirl::{CreateOptions, Event, Ownership, ResourceState};
use common::{TestDir, assert_disk_matches_registry, backdate};

/// 8 workers x 24 attempts = 192 creates against a manager-wide cap of
/// 40 resources. Every attempt past the cap is a quota rejection, so at
/// least a third of all attempts must be refused — no timing luck
/// involved.
const S1_WORKERS: usize = 8;
const S1_ITERS: usize = 24;
const S1_MAX_RESOURCES: u64 = 40;

#[test]
fn create_release_under_quota_pressure_stays_consistent() {
    let dir = TestDir::new("s1-create-release");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s1")
        .max_resources(S1_MAX_RESOURCES)
        .build()
        .unwrap();
    let ns = manager.namespace("hot").unwrap();

    let barrier = Arc::new(Barrier::new(S1_WORKERS));
    let mut handles = Vec::with_capacity(S1_WORKERS);
    for worker in 0..S1_WORKERS {
        let ns = ns.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut ok = Vec::new();
            let mut rejected = 0u32;
            for i in 0..S1_ITERS {
                // files and buffers alternate deterministically
                let outcome = if (worker + i) % 2 == 0 {
                    ns.create_file(&format!("w-{worker}-{i}.bin"))
                        .map(|f| f.id().clone())
                } else {
                    ns.create_buffer(128).map(|b| b.id().clone())
                };
                match outcome {
                    Ok(id) => ok.push(id),
                    Err(cachegirl::Error::Quota(_)) => rejected += 1,
                    Err(other) => panic!("unexpected create error: {other}"),
                }
            }
            (ok, rejected)
        }));
    }
    let mut created_ids = Vec::new();
    let mut rejected_total = 0u64;
    for handle in handles {
        let (ids, rejected) = handle.join().unwrap();
        created_ids.extend(ids);
        rejected_total += rejected as u64;
    }

    let created = created_ids.len() as u64;
    // nobody released anything during the storm, so the cap is exactly filled
    assert_eq!(created, S1_MAX_RESOURCES);
    assert_eq!(created + rejected_total, (S1_WORKERS * S1_ITERS) as u64);
    // the registry reflects exactly the successful creations — not one more
    assert_eq!(manager.registry().len().unwrap() as u64, created);
    assert_disk_matches_registry(&manager);

    // release everything; the workspace must go quiet again
    for id in &created_ids {
        assert!(ns.release(id).unwrap(), "release must accept a live id");
    }
    assert_eq!(manager.registry().len().unwrap(), 0);
    assert_disk_matches_registry(&manager);
    manager.shutdown().unwrap();
}

/// 100 hand-offs of a leased resource: one racing release, one racing
/// cleanup pass, both forbidden to win. Loop-scoped instead of
/// sleep-scoped — the assertion holds on every pass, not just in the
/// lucky run.
const S2_ITERATIONS: usize = 100;

#[test]
fn release_and_cleanup_never_win_a_held_lease() {
    let dir = TestDir::new("s2-lease-race");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s2")
        .build()
        .unwrap();
    let ns = manager.namespace("leases").unwrap();

    for iter in 0..S2_ITERATIONS {
        let name = format!("held-{iter}.bin");
        let file = ns
            .create_file_with(&name, CreateOptions::new().ttl(Duration::from_secs(60)))
            .unwrap();
        // overdue on paper, so the sweeper genuinely wants this one
        backdate(&manager, file.id(), 7200);
        let id = file.id().clone();
        let path = file.path().to_path_buf();
        let lease = file.acquire().unwrap();
        // park the record deliberately (release with a live lease), so
        // the two threads race on an already-parked resource — that
        // removes the benign state-transition race between the sweeper
        // and the releasing thread and keeps the outcome deterministic
        assert!(!ns.release(&id).unwrap());
        assert_eq!(
            manager.registry().get(&id).unwrap().state,
            ResourceState::PendingCleanup
        );
        let barrier = Barrier::new(2);

        std::thread::scope(|scope| {
            let release_handle = scope.spawn(|| {
                barrier.wait();
                ns.release(&id).unwrap()
            });
            let cleanup_handle = scope.spawn(|| {
                barrier.wait();
                manager.cleanup_expired().unwrap()
            });
            let released = release_handle.join().unwrap();
            let report = cleanup_handle.join().unwrap();
            assert!(!released, "release destroyed a resource with a live lease");
            assert_eq!(
                report.removed, 0,
                "cleanup destroyed a resource with a live lease"
            );
            assert_eq!(report.deferred, 1, "a leased resource must be deferred");
        });

        assert!(path.exists(), "the leased file disappeared from disk");
        let meta = manager.registry().get(&id).unwrap();
        assert_eq!(meta.state, ResourceState::PendingCleanup);

        drop(lease);
        // lease gone; the sweeper collects
        let report = manager.cleanup_expired().unwrap();
        assert_eq!(report.removed, 1);
        assert!(!path.exists());
    }
    assert_eq!(manager.registry().len().unwrap(), 0);
    assert_disk_matches_registry(&manager);
    manager.shutdown().unwrap();
}

/// Four workers hammering create/refresh/release and the read path,
/// while a janitor thread sweeps in the background. Nothing here has a
/// TTL, so the sweeper must never find a victim — its only jobs are to
/// not deadlock and to not damage live resources. 1120 creates plus
/// 40 sweep passes, all bounded, no sleeps.
const S3_WORKERS: usize = 4;
const S3_OPS_PER_WORKER: usize = 280;
const S3_JANITOR_PASSES: usize = 40;

#[test]
fn mixed_load_create_cleanup_refresh_cache_get() {
    let dir = TestDir::new("s3-mixed");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s3")
        .build()
        .unwrap();
    let ns = manager.namespace("mix").unwrap();

    let barrier = Arc::new(Barrier::new(S3_WORKERS + 1));
    let pool = Arc::new(Mutex::new(Vec::new()));
    let created = Arc::new(std::sync::atomic::AtomicU64::new(0));

    std::thread::scope(|scope| {
        for worker in 0..S3_WORKERS {
            let ns = ns.clone();
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            let created = Arc::clone(&created);
            scope.spawn(move || {
                barrier.wait();
                for i in 0..S3_OPS_PER_WORKER {
                    let outcome = if i % 2 == 0 {
                        ns.create_file(&format!("mix-w-{worker}-{i}.bin"))
                            .map(|f| f.id().clone())
                    } else {
                        ns.create_buffer(64 + (i % 32)).map(|b| b.id().clone())
                    };
                    let id = outcome.unwrap_or_else(|e| panic!("create failed: {e}"));
                    created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // refresh must succeed on a resource that is alive here
                    ns.refresh_size(&id).unwrap();
                    pool.lock().unwrap().push(id);
                    // every third op retires one pooled resource
                    if i % 3 == 0 {
                        if let Some(id) = pool.lock().unwrap().pop() {
                            let _ = ns.release(&id).unwrap();
                        }
                    }
                    // exercise the read path on a mostly-absent key space
                    let _ = ns.cache_get(&format!("missing-{}", i % 8)).unwrap();
                }
            });
        }
        {
            let manager = &manager;
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..S3_JANITOR_PASSES {
                    let report = manager.cleanup_expired().unwrap();
                    assert_eq!(report.failed, 0, "the sweep must not fail");
                    assert_eq!(
                        report.removed, 0,
                        "nothing here has a TTL; the sweep must find nothing"
                    );
                }
            });
        }
    });

    let total = created.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(total, (S3_WORKERS * S3_OPS_PER_WORKER) as u64);

    // drain whatever the workers left behind
    let rest: Vec<cachegirl::ResourceId> = pool.lock().unwrap().drain(..).collect();
    for id in rest {
        let _ = ns.release(&id).unwrap();
    }
    assert_eq!(manager.registry().len().unwrap(), 0);
    assert_disk_matches_registry(&manager);
    manager.shutdown().unwrap();
}

/// Six co-owners of one shared file, dropped from six threads; one of
/// them panics on the way out. Thread scheduling supplies the random
/// drop order. Exactly one destruction may happen in total, and it must
/// happen at the very last owner — whoever that turns out to be.
const S4_CLONES: usize = 6;

#[test]
fn shared_owners_drop_in_any_order_panic_included() {
    let dir = TestDir::new("s4-shared");
    let sink = Arc::new(cachegirl::VecSink::new());
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s4")
        .event_sink(sink.clone())
        .build()
        .unwrap();
    let ns = manager.namespace("pair").unwrap();

    let first = ns
        .create_file_with(
            "shared.bin",
            CreateOptions::new().ownership(Ownership::Shared),
        )
        .unwrap();
    let path = first.path().to_path_buf();
    let id = first.id().clone();
    std::fs::write(&path, b"shared payload").unwrap();

    let mut handles = Vec::with_capacity(S4_CLONES);
    for idx in 0..S4_CLONES {
        let clone = first.clone();
        handles.push(std::thread::spawn(move || {
            // read through our own handle, then drop it
            assert_eq!(std::fs::read(clone.path()).unwrap(), b"shared payload");
            if idx == 0 {
                // unwinding drops `clone` on the way out
                panic!("owner {idx} blew up");
            }
            drop(clone);
        }));
    }
    let mut panicked = 0;
    for handle in handles {
        if handle.join().is_err() {
            panicked += 1;
        }
    }
    assert_eq!(panicked, 1, "exactly the designated owner must panic");

    // all co-owners are gone; the main-thread owner keeps the file alive
    assert!(path.exists());
    assert!(manager.registry().get(&id).is_ok());

    // the last owner destroys the file — exactly once
    drop(first);
    assert!(!path.exists());
    assert!(manager.registry().get(&id).is_err());
    let events = sink.drain();
    let released = events
        .iter()
        .filter(|e| matches!(e, Event::ResourceReleased { id: ev, .. } if *ev == id))
        .count();
    assert_eq!(released, 1, "shared destruction must happen exactly once");
    assert_disk_matches_registry(&manager);
    manager.shutdown().unwrap();
}

/// Poison the registry from under its lock and see what the manager
/// does: every registry-backed operation must fail fast with
/// `Poisoned`, never hang, never delete the file spuriously. This is
/// the documented blast radius of `Mutex` poisoning in this codebase —
/// it wedges the registry, not the process.
#[test]
fn poisoned_registry_fails_fast_instead_of_hanging() {
    let dir = TestDir::new("s5-poison");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s5")
        .build()
        .unwrap();
    let ns = manager.namespace("poison").unwrap();
    let file = ns.create_file("doomed.bin").unwrap();
    let id = file.id().clone();
    let path = file.path().to_path_buf();

    // boom while the registry lock is held; unwinding poisons the mutex
    let registry = manager.registry();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = registry.update(&id, |_| panic!("boom under the registry lock"));
    }));

    assert!(matches!(
        manager.registry().len(),
        Err(cachegirl::RegistryError::Poisoned)
    ));

    // a worker thread hitting the registry sees the error promptly —
    // this test hanging would be the failure mode
    let ns2 = ns.clone();
    let id2 = id.clone();
    let result = std::thread::spawn(move || ns2.release(&id2))
        .join()
        .unwrap();
    assert!(matches!(
        result,
        Err(cachegirl::Error::Registry(
            cachegirl::RegistryError::Poisoned
        ))
    ));

    // the physical resource is left alone: poisoning never deletes files
    assert!(path.exists());

    // the sweeper needs the same registry; it must report the failure, not panic
    assert!(manager.cleanup_expired().is_err());

    // shutdown without the cleanup sweep never touches the registry — still fine
    assert!(manager.shutdown().is_ok());
}

/// 8 threads x 25 mixed cache operations against a 16-key space.
/// Creates of the same key race; exactly one wins per key and the rest
/// must be rejected cleanly. The store must come out with every write
/// accounted for and every read byte-identical to the winning write.
const S6_WORKERS: usize = 8;
const S6_OPS_PER_WORKER: usize = 50;
const S6_KEYS: usize = 16;

#[test]
fn concurrent_cache_access_never_corrupts_the_store() {
    let dir = TestDir::new("s6-cache");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s6")
        .build()
        .unwrap();
    let ns = manager.namespace("cache").unwrap();

    let barrier = Arc::new(Barrier::new(S6_WORKERS));
    let wins = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::with_capacity(S6_WORKERS);
    for worker in 0..S6_WORKERS {
        let ns = ns.clone();
        let barrier = Arc::clone(&barrier);
        let wins = Arc::clone(&wins);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for i in 0..S6_OPS_PER_WORKER {
                // one key space for everyone; i % 3 == 0 reads, the rest writes
                let key = format!("key-{}", i % S6_KEYS);
                let value = vec![worker as u8 + 1; 8];
                if i % 3 == 0 {
                    // reads never error out, whatever they find or miss
                    let _ = ns.cache_get(&key).unwrap();
                } else {
                    match ns.create_cache_entry(&key, value.clone()) {
                        Ok(_) => wins.lock().unwrap().push((key, value)),
                        Err(cachegirl::Error::Backend(cachegirl::BackendError::Rejected {
                            ..
                        })) => {} // someone else got there first — fine
                        Err(other) => panic!("unexpected create error: {other}"),
                    }
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }

    let wins = Arc::try_unwrap(wins).unwrap().into_inner().unwrap();
    let mut wins_per_key: HashMap<String, usize> = HashMap::new();
    for (key, _) in &wins {
        *wins_per_key.entry(key.clone()).or_default() += 1;
    }
    // exactly one winner per key — no double-inserts, no lost creates
    assert_eq!(
        wins.len(),
        S6_KEYS,
        "every key must be created exactly once"
    );
    assert_eq!(wins_per_key.len(), S6_KEYS);
    assert!(wins_per_key.values().all(|n| *n == 1));

    assert_eq!(manager.registry().len().unwrap(), S6_KEYS);
    // reads return exactly what the winning create wrote
    for (key, value) in &wins {
        let entry = ns
            .cache_get(key)
            .unwrap()
            .unwrap_or_else(|| panic!("key {key} went missing"));
        assert_eq!(
            &entry.value, value,
            "stored value diverged from the winning write"
        );
    }
    let stats = manager.stats().unwrap();
    assert_eq!(stats.resources, S6_KEYS as u64);
    assert_eq!(stats.bytes_tracked, (S6_KEYS * 8) as u64);

    // releasing everything empties the store
    let metas = manager.registry().snapshot().unwrap();
    for meta in metas {
        assert!(ns.release(&meta.id).unwrap());
    }
    assert_eq!(manager.registry().len().unwrap(), 0);
    for i in 0..S6_KEYS {
        assert!(ns.cache_get(&format!("key-{i}")).unwrap().is_none());
    }
    manager.shutdown().unwrap();
}

/// Eight threads call shutdown at the same instant, on a manager whose
/// shutdown is configured to sweep. Exactly one caller may run the
/// sweep; the others get a calm `Ok`. Every resource must be destroyed
/// exactly once — no double destruction, no survivor that should have
/// been collected.
const S7_THREADS: usize = 8;

#[test]
fn concurrent_shutdown_sweeps_once_and_destroys_nothing_twice() {
    let dir = TestDir::new("s7-shutdown");
    let sink = Arc::new(cachegirl::VecSink::new());
    let manager = Arc::new(
        cachegirl::TempManager::builder()
            .root(dir.path())
            .instance_id("instance-s7")
            .cleanup_on_shutdown(true)
            .event_sink(sink.clone())
            .build()
            .unwrap(),
    );
    let ns = manager.namespace("sweep").unwrap();

    // three expired tenants
    let mut expired_ids = Vec::new();
    for i in 0..3 {
        let file = ns
            .create_file_with(
                &format!("expired-{i}.bin"),
                CreateOptions::new().ttl(Duration::from_secs(60)),
            )
            .unwrap();
        backdate(&manager, file.id(), 7200);
        expired_ids.push(file.id().clone());
    }
    // one lease-parked pending tenant: parked before shutdown, lease gone by then
    let parked = ns
        .create_file_with(
            "parked.bin",
            CreateOptions::new().ttl(Duration::from_secs(60)),
        )
        .unwrap();
    backdate(&manager, parked.id(), 7200);
    let parked_id = parked.id().clone();
    let lease = parked.acquire().unwrap();
    assert!(!ns.release(&parked_id).unwrap()); // parked, not destroyed
    drop(lease);
    // two fresh in-ttl files that must ride out the sweep
    for i in 0..2 {
        ns.create_file_with(
            &format!("fresh-{i}.bin"),
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    }

    let barrier = Arc::new(Barrier::new(S7_THREADS));
    let mut handles = Vec::with_capacity(S7_THREADS);
    for _ in 0..S7_THREADS {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            manager.shutdown()
        }));
    }
    let reports: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let failures: Vec<_> = reports.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failures.is_empty(),
        "every concurrent shutdown must succeed: {failures:?}"
    );

    // exactly one caller ran the sweep; everyone else no-op'd calmly
    let swept = reports
        .iter()
        .filter(|r| r.as_ref().unwrap().scanned > 0)
        .count();
    assert_eq!(swept, 1, "exactly one shutdown may run the sweep");
    let removed: u64 = reports.iter().map(|r| r.as_ref().unwrap().removed).sum();
    assert_eq!(
        removed, 4,
        "three expired + one parked must all be collected"
    );

    // exactly-once destruction: one release event per destroyed id
    let events = sink.drain();
    for id in expired_ids.iter().chain(std::iter::once(&parked_id)) {
        let count = events
            .iter()
            .filter(|e| matches!(e, Event::ResourceReleased { id: ev, .. } if ev == id))
            .count();
        assert_eq!(count, 1, "resource {id} destroyed more than once");
    }
    // the fresh files are untouched, the registry and disk agree
    let metas = manager.registry().snapshot().unwrap();
    assert_eq!(metas.len(), 2, "only the fresh resources may survive");
    assert_disk_matches_registry(&manager);
}

/// Four creators hammer out files while a janitor sweeps. Half the
/// files carry a TTL of zero — legal sweep targets at any instant —
/// and half carry a one-hour TTL — untouchable this pass. One extra
/// zero-TTL file is held under a lease for the whole run and must
/// survive every sweep. Nobody lives forever that shouldn't: after the
/// lease drops and one final sweep runs, every zero-TTL record is gone
/// and exactly the one-hour cohort remains.
const S8_CREATORS: usize = 4;
const S8_FILES_PER_CREATOR: usize = 15; // half volatile, half durable
const S8_JANITOR_PASSES: usize = 30;

#[test]
fn cleanup_under_creation_pressure_respects_ttls_and_leases() {
    let dir = TestDir::new("s8-gc");
    let manager = cachegirl::TempManager::builder()
        .root(dir.path())
        .instance_id("instance-s8")
        .build()
        .unwrap();
    let ns = manager.namespace("gc").unwrap();

    // one volatile file pinned under a lease for the whole janitor run
    let pinned = ns
        .create_file_with("pinned.bin", CreateOptions::new().ttl(Duration::ZERO))
        .unwrap();
    let pinned_id = pinned.id().clone();
    let pinned_path = pinned.path().to_path_buf();
    let lease = pinned.acquire().unwrap();

    let barrier = Arc::new(Barrier::new(S8_CREATORS + 1));
    let durable_ids = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for creator in 0..S8_CREATORS {
            let ns = ns.clone();
            let barrier = Arc::clone(&barrier);
            let durable_ids = Arc::clone(&durable_ids);
            scope.spawn(move || {
                barrier.wait();
                for i in 0..S8_FILES_PER_CREATOR {
                    // alternate volatile and durable by index — deterministic
                    if i % 2 == 0 {
                        let volatile = ns.create_file_with(
                            &format!("volatile-{creator}-{i}.bin"),
                            CreateOptions::new().ttl(Duration::ZERO),
                        );
                        let volatile = volatile.unwrap_or_else(|e| panic!("create failed: {e}"));
                        // managed drop keeps the record; the sweeper may take it any time
                        drop(volatile);
                    } else {
                        let durable = ns.create_file_with(
                            &format!("durable-{creator}-{i}.bin"),
                            CreateOptions::new().ttl(Duration::from_secs(3600)),
                        );
                        let durable = durable.unwrap_or_else(|e| panic!("create failed: {e}"));
                        durable_ids.lock().unwrap().push(durable.id().clone());
                        drop(durable); // managed drop keeps the record
                    }
                }
            });
        }
        {
            let manager = &manager;
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..S8_JANITOR_PASSES {
                    let report = manager.cleanup_expired().unwrap();
                    assert_eq!(report.failed, 0, "the sweep must not fail");
                }
            });
        }
    });

    // the pinned file survived every single pass, readable and pending
    assert!(
        pinned_path.exists(),
        "a leased file was swept despite the lease"
    );
    assert_eq!(
        manager.registry().get(&pinned_id).unwrap().state,
        ResourceState::PendingCleanup
    );

    // the durable cohort was never touched
    let durable_ids = durable_ids.lock().unwrap().clone();
    assert_eq!(durable_ids.len(), S8_CREATORS * (S8_FILES_PER_CREATOR / 2));
    for id in &durable_ids {
        let meta = manager.registry().get(id).unwrap();
        assert_eq!(meta.state, ResourceState::Created);
    }

    // with the lease gone, one final sweep collects every zero-TTL record
    drop(lease);
    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.failed, 0);
    assert!(!pinned_path.exists());

    // exactly the durable cohort may remain
    let remaining = manager.registry().snapshot().unwrap();
    assert_eq!(remaining.len(), durable_ids.len());
    assert!(
        remaining
            .iter()
            .all(|m| { m.ttl == Some(cachegirl::Ttl::Fixed(Duration::from_secs(3600))) })
    );
    assert_disk_matches_registry(&manager);

    // and once the durable cohort is released, the workspace is empty
    for id in &durable_ids {
        assert!(ns.release(id).unwrap());
    }
    assert_eq!(manager.registry().len().unwrap(), 0);
    assert_disk_matches_registry(&manager);
    manager.shutdown().unwrap();
}
