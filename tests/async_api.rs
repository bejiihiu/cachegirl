//! Async API integration tests.
//!
//! Gate: `feature = "async"`. Under `--all-features` (or `--features
//! tokio`) these exercise the blocking-pool path; under plain `--features
//! async` they exercise the inline path. Behavior must be identical.

#![cfg(feature = "async")]

use std::sync::Arc;

use cachegirl::AsyncTempManager;

fn unique_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cachegirl-async-{tag}-{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    ))
}

/// Minimal executor that never touches a runtime: proves the facade
/// works with zero ambient machinery.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll};

    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            // our futures are ready immediately; yield anyway so a
            // genuinely pending future cannot starve the loop hard
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn create_and_release_file_roundtrip() {
    let dir = unique_dir("roundtrip");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-roundtrip")
        .build()
        .await
        .unwrap();

    let ns = manager.namespace("protocol").await.unwrap();
    let file = ns.create_file("packet-dump.bin").await.unwrap();
    assert!(file.path().is_file());

    let id = file.id().clone();
    assert!(ns.release(&id).await.unwrap());
    assert!(!file.path().exists());

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test(flavor = "current_thread")]
async fn current_thread_runtime_is_supported() {
    // spawn_blocking on the current-thread runtime: the blocking pool is
    // shared, not per-worker, so this must not deadlock or panic
    let dir = unique_dir("current-thread");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-current-thread")
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("net").await.unwrap();
    let file = ns.create_file("single.bin").await.unwrap();
    assert!(file.path().exists());
    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_tasks_respect_manager_quota() {
    let dir = unique_dir("concurrent");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-concurrent")
        .max_resources(20)
        .build()
        .await
        .unwrap();
    let ns = Arc::new(manager.namespace("parallel").await.unwrap());

    let mut handles = Vec::new();
    for worker in 0..6 {
        let ns = Arc::clone(&ns);
        handles.push(tokio::spawn(async move {
            let mut created = 0u32;
            let mut rejected = 0u32;
            for n in 0..10 {
                match ns.create_file(&format!("worker-{worker}-{n}.bin")).await {
                    Ok(_) => created += 1,
                    Err(cachegirl::Error::Quota(_)) => rejected += 1,
                    Err(other) => panic!("unexpected error: {other}"),
                }
            }
            (created, rejected)
        }));
    }

    let mut created = 0u32;
    let mut rejected = 0u32;
    for handle in handles {
        let (c, r) = handle.await.unwrap();
        created += c;
        rejected += r;
    }
    assert_eq!(created, 20);
    assert_eq!(rejected, 40);
    assert_eq!(manager.registry().len().unwrap(), 20);

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn concurrent_create_and_release_across_tasks() {
    let dir = unique_dir("create-release");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-cr")
        .build()
        .await
        .unwrap();
    let ns = Arc::new(manager.namespace("churn").await.unwrap());

    let mut handles = Vec::new();
    for worker in 0..8 {
        let ns = Arc::clone(&ns);
        handles.push(tokio::spawn(async move {
            for n in 0..5 {
                let file = ns
                    .create_file(&format!("drop-{worker}-{n}.bin"))
                    .await
                    .unwrap();
                let id = file.id().clone();
                assert!(ns.release(&id).await.unwrap());
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(manager.registry().len().unwrap(), 0);
    let stats = manager.stats().await.unwrap();
    assert_eq!(stats.resources_released, 40);

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "memory")]
#[tokio::test]
async fn buffer_creation_and_stats_are_awaitable() {
    let dir = unique_dir("buffer");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-buffer")
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("memory").await.unwrap();

    let mut buffer = ns.create_buffer(1024).await.unwrap();
    buffer.extend_from_slice(b"hello");
    assert_eq!(buffer.len(), 5);

    let stats = manager.stats().await.unwrap();
    assert_eq!(stats.resources, 1);
    assert_eq!(stats.bytes_tracked, 1024);

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(all(feature = "cache", feature = "memory"))]
#[tokio::test]
async fn cache_roundtrip_over_async_api() {
    let dir = unique_dir("cache");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-cache")
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("cache").await.unwrap();

    ns.create_cache_entry("pkt-1", vec![1, 2, 3]).await.unwrap();
    let fetched = ns.cache_get("pkt-1").await.unwrap().expect("entry present");
    assert_eq!(fetched.value, vec![1, 2, 3]);

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn cleanup_expired_reclaims_backdated_resource() {
    let dir = unique_dir("cleanup");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-cleanup")
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("sweep").await.unwrap();

    let file = ns.create_file("stale.bin").await.unwrap();
    let id = file.id().clone();
    // pin a creation time far in the past so the default policy sweeps it
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    manager
        .registry()
        .update(&id, |m| m.created_at = past)
        .unwrap();

    // expire by TTL: the file carries no ttl of its own, so give the
    // namespace a cleanup policy instead
    ns.set_cleanup_policy(
        cachegirl::CleanupPolicy::builder()
            .ttl(std::time::Duration::from_secs(1))
            .build(),
    )
    .await;

    let report = manager.cleanup_expired().await.unwrap();
    assert_eq!(report.removed, 1);
    assert!(!file.path().exists());

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn namespace_scoped_cleanup_leaves_other_namespaces_alone() {
    let dir = unique_dir("ns-cleanup");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-ns-cleanup")
        .build()
        .await
        .unwrap();
    let swept = manager.namespace("swept").await.unwrap();
    let untouched = manager.namespace("untouched").await.unwrap();

    let stale = swept.create_file("stale.bin").await.unwrap();
    let kept = untouched.create_file("kept.bin").await.unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    manager
        .registry()
        .update(stale.id(), |m| m.created_at = past)
        .unwrap();
    manager
        .registry()
        .update(kept.id(), |m| m.created_at = past)
        .unwrap();

    swept
        .set_cleanup_policy(
            cachegirl::CleanupPolicy::builder()
                .ttl(std::time::Duration::from_secs(1))
                .build(),
        )
        .await;

    let report = swept.cleanup_expired().await.unwrap();
    assert_eq!(report.removed, 1);
    assert!(!stale.path().exists());
    assert!(kept.path().exists());

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn startup_recovery_runs_through_the_async_builder() {
    let dir = unique_dir("recovery");
    // plant a dead instance: lock names a pid that cannot exist
    let dead = dir.join("instance-crashed");
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::write(
        dead.join(".cachegirl.lock"),
        "version=2\npid=2147483646\ncreated=1763100000\ninstance=instance-crashed\n",
    )
    .unwrap();
    std::fs::write(dead.join("data.bin"), vec![b'x'; 256]).unwrap();

    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-recovery")
        .cleanup_on_start(true)
        .build()
        .await
        .unwrap();

    let report = manager
        .recovery_report()
        .expect("startup must produce a report");
    assert_eq!(report.removed, 1, "{report}");
    assert!(!dead.exists());

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn creation_fails_after_async_shutdown() {
    let dir = unique_dir("shutdown");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-shutdown")
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("closing").await.unwrap();
    manager.shutdown().await.unwrap();
    assert!(manager.is_closed());

    let err = ns.create_file("too-late.bin").await.unwrap_err();
    assert!(matches!(
        err,
        cachegirl::Error::Concurrency(cachegirl::ConcurrencyError::ManagerClosed)
    ));
    // a second shutdown is fine, and it is still reported as closed
    manager.shutdown().await.unwrap();
    assert!(manager.namespace("nope").await.is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "filesystem")]
#[tokio::test]
async fn shutdown_with_cleanup_on_shutdown_sweeps_expired() {
    let dir = unique_dir("shutdown-sweep");
    let manager = AsyncTempManager::builder()
        .root(&dir)
        .instance_id("async-shutdown-sweep")
        .cleanup_on_shutdown(true)
        .build()
        .await
        .unwrap();
    let ns = manager.namespace("closing").await.unwrap();

    let file = ns.create_file("doomed.bin").await.unwrap();
    let id = file.id().clone();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    manager
        .registry()
        .update(&id, |m| m.created_at = past)
        .unwrap();
    ns.set_cleanup_policy(
        cachegirl::CleanupPolicy::builder()
            .ttl(std::time::Duration::from_secs(1))
            .build(),
    )
    .await;

    manager.shutdown().await.unwrap();
    assert!(!file.path().exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// No runtime anywhere: the facade must still work, inline.
#[cfg(feature = "filesystem")]
#[test]
fn async_api_works_without_any_runtime() {
    let dir = unique_dir("no-runtime");
    let result = block_on(async {
        let manager = AsyncTempManager::builder()
            .root(&dir)
            .instance_id("async-no-runtime")
            .build()
            .await?;
        let ns = manager.namespace("plain").await?;
        let file = ns.create_file("no-runtime.bin").await?;
        let id = file.id().clone();
        ns.release(&id).await?;
        manager.shutdown().await?;
        Ok::<_, cachegirl::Error>(())
    });
    result.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// into_inner / as_inner round trip keeps one shared state.
#[cfg(feature = "filesystem")]
#[test]
fn async_handles_unwrap_to_sync_core() {
    let dir = unique_dir("unwrap");
    let result = block_on(async {
        let manager = AsyncTempManager::builder()
            .root(&dir)
            .instance_id("async-unwrap")
            .build()
            .await?;
        assert_eq!(manager.as_inner().instance_id(), "async-unwrap");
        let sync = manager.into_inner();
        Ok::<_, cachegirl::Error>(sync)
    });
    let sync = result.unwrap();
    sync.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
