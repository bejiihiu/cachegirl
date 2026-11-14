//! Every Rust snippet from `docs/*.md`, compiled and run against the
//! public API. Kept in sync with the guides; if a snippet changes in a
//! guide, change it here in the same commit.

// сниппеты используют сразу несколько необязательных частей API — файловые операции, буферы, кэш и async façade
#![cfg(all(
    feature = "filesystem",
    feature = "memory",
    feature = "cache",
    feature = "async",
))]
#![allow(dead_code, unused_imports)]

mod snippet_cleanup_policies_1 {
    use cachegirl::{TempManager, Ttl};
    use std::time::Duration;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-ttl-{}", std::process::id()));
        let manager = TempManager::builder()
            .root(&dir)
            .default_ttl(Duration::from_secs(3600)) // baked into every record as Ttl::Fixed
            .build()?;
        let ns = manager.namespace("demo")?;

        let meta = manager.registry().get(ns.create_file("probe.bin")?.id())?;
        println!("default ttl: {:?}", meta.ttl);

        println!(
            "fixed expired now? {}",
            Ttl::Fixed(Duration::from_secs(0)).is_expired(
                meta.created_at,
                meta.last_accessed,
                std::time::SystemTime::now()
            )
        );

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_cleanup_policies_1() {
    snippet_cleanup_policies_1::main().expect("snippet failed");
}

mod snippet_cleanup_policies_2 {
    use cachegirl::{CleanupPolicy, Priority, TempManager};
    use std::time::Duration;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-policy-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;

        // sweep after 5 minutes of idleness, cap usage at 64 MiB / 1000
        // resources, and make this the default priority for new resources
        manager.set_cleanup_policy(
            CleanupPolicy::builder()
                .idle_ttl(Duration::from_secs(300))
                .max_size(64 * 1024 * 1024)
                .max_resources(1_000)
                .priority(Priority::Normal)
                .build(),
        );

        let ns = manager.namespace("demo")?;
        let report = ns.create_file("probe.bin")?;
        println!("created {}", report.id());
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_cleanup_policies_2() {
    snippet_cleanup_policies_2::main().expect("snippet failed");
}

mod snippet_cleanup_policies_3 {
    use cachegirl::{CleanupContext, CreateOptions, PolicyDecision, ResourceMeta, TempManager};
    use std::sync::Arc;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-custom-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("scratch")?;

        // closure form: closures with this signature implement the trait
        manager.set_custom_cleanup_policy(Arc::new(
            |meta: &ResourceMeta, _ctx: &CleanupContext| match meta
                .metadata
                .get("keep")
                .map(String::as_str)
            {
                Some("true") => PolicyDecision::Keep,
                _ => PolicyDecision::Remove,
            },
        ));

        ns.create_file_with(
            "precious.bin",
            CreateOptions::new().metadata("keep", "true"),
        )?;
        ns.create_file("temp.bin")?;

        let report = manager.cleanup_expired()?;
        println!(
            "policy sweep removed {} ({:?})",
            report.removed, report.reasons.policy
        );

        manager.clear_custom_cleanup_policy();
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_cleanup_policies_3() {
    snippet_cleanup_policies_3::main().expect("snippet failed");
}

mod snippet_cleanup_policies_4 {
    use cachegirl::TempManager;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-report-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let report = manager.cleanup_expired()?;

        println!("scanned={} removed={}", report.scanned, report.removed);
        println!(
            "skipped={} failed={} deferred={}",
            report.skipped, report.failed, report.deferred
        );
        println!(
            "bytes_freed={} duration={:?}",
            report.bytes_freed, report.duration
        );
        println!(
            "reasons: ttl_fixed={} ttl_idle={} size_eviction={} count_eviction={} policy={} lease_deferred={} eligible={}",
            report.reasons.ttl_fixed,
            report.reasons.ttl_idle,
            report.reasons.size_eviction,
            report.reasons.count_eviction,
            report.reasons.policy,
            report.reasons.lease_deferred,
            report.reasons.eligible
        );

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_cleanup_policies_4() {
    snippet_cleanup_policies_4::main().expect("snippet failed");
}

mod snippet_configuration_1 {
    use std::sync::Arc;
    use std::time::Duration;

    use cachegirl::{CleanupPolicy, NullSink, RecoveryPolicy, SymlinkPolicy, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-config-{}", std::process::id()));

        let manager = TempManager::builder()
            .root(&dir) // required
            .create_if_missing(true) // default true
            .cleanup_on_start(true) // default false
            .cleanup_on_shutdown(true) // default false
            .max_size(1024 * 1024 * 1024) // default: unlimited
            .max_resources(10_000) // default: unlimited
            .default_ttl(Duration::from_secs(3600)) // default: no TTL
            .symlink_policy(SymlinkPolicy::Reject) // default: Reject
            .instance_id("my-service-1") // default: random
            .recovery_policy(RecoveryPolicy::OnStartup) // default: OnStartup
            .stale_instance_after(Duration::from_secs(3600)) // default: 1 hour
            .event_sink(Arc::new(NullSink)) // default: NullSink
            .cleanup_policy(CleanupPolicy::builder().max_resources(100).build())
            .build()?;

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_configuration_1() {
    snippet_configuration_1::main().expect("snippet failed");
}

mod snippet_crash_recovery_1 {
    use cachegirl::{RecoveryAction, RecoveryManager, RecoveryPolicy};

    pub fn main() -> cachegirl::Result<()> {
        let root = std::env::temp_dir().join(format!("cachegirl-recovery-{}", std::process::id()));

        // plant a fake stale instance: a directory with a lock whose pid
        // is far beyond any realistic pid on this machine
        let dead = root.join("instance-crashed");
        std::fs::create_dir_all(&dead).expect("fake instance dir");
        std::fs::write(
            dead.join(".cachegirl.lock"),
            format!(
                "version=2\npid={}\ncreated=1763100000\ninstance=instance-crashed\n",
                0x7FFF_FFFEu32
            ),
        )
        .expect("fake lock");
        std::fs::write(dead.join("data.bin"), vec![b'x'; 256]).expect("fake payload");

        // diagnose first
        let recovery = RecoveryManager::new(&root);
        let report = recovery.recover(RecoveryPolicy::ReportOnly, "instance-current")?;
        println!("report-only: {report}");
        assert!(dead.exists());

        // then reclaim
        let report = recovery.recover(RecoveryPolicy::OnStartup, "instance-current")?;
        for entry in &report.entries {
            println!(
                "  {} -> {:?}: {}",
                entry.instance, entry.action, entry.reason
            );
        }
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.action == RecoveryAction::Removed)
        );
        assert!(!dead.exists());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}

#[test]
fn snippet_crash_recovery_1() {
    snippet_crash_recovery_1::main().expect("snippet failed");
}

mod snippet_error_handling_1 {
    use cachegirl::{Error, QuotaError, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-errors-{}", std::process::id()));
        let manager = match TempManager::builder().root(&dir).build() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("startup failed: {e}");
                return Err(e);
            }
        };

        match manager.namespace("api") {
            Ok(ns) => match ns.create_file("scratch.bin") {
                Ok(file) => println!("created {}", file.id()),
                Err(Error::Quota(QuotaError::CountExceeded { limit })) => {
                    // expected backpressure: shed load or grow capacity
                    eprintln!("at the count quota ({limit}); refusing new scratch file");
                }
                Err(e) => eprintln!("unexpected creation failure: {e}"),
            },
            Err(e) => eprintln!("namespace failure: {e}"),
        }

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_error_handling_1() {
    snippet_error_handling_1::main().expect("snippet failed");
}

mod snippet_getting_started_1 {
    use cachegirl::TempManager;

    pub fn main() -> cachegirl::Result<()> {
        // pick a root of your own; the library never imposes one
        let dir = std::env::temp_dir().join(format!("cachegirl-guide-{}", std::process::id()));

        let manager = TempManager::builder()
            .root(&dir)
            .create_if_missing(true) // create the root if it does not exist
            .cleanup_on_shutdown(true) // sweep expired resources on shutdown
            .build()?;

        // resources live in namespaces
        let scratch = manager.namespace("scratch")?;

        // files are written through the path the handle gives you
        let file = scratch.create_file("packet-dump.bin")?;
        std::fs::write(file.path(), b"hello cachegirl").expect("file is writable");

        // the registry tracks size at creation; re-stat to sync it after writes
        let tracked = scratch.refresh_size(file.id())?;
        println!("tracked {tracked} bytes");

        // explicit release when the application is done with the resource
        scratch.release(file.id())?;

        // an explicit cleanup pass; the library never spawns background threads
        let report = manager.cleanup_expired()?;
        println!("cleanup: {report}");

        // orderly shutdown: sweep (if configured) + lock file removal
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_getting_started_1() {
    snippet_getting_started_1::main().expect("snippet failed");
}

mod snippet_namespaces_1 {
    use cachegirl::{NamespaceQuota, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-ns-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;

        let network = manager.namespace("network")?;
        let again = manager.namespace("network")?;
        assert!(std::sync::Arc::ptr_eq(&network, &again));

        network.set_quota(NamespaceQuota::with_max_bytes(4096));
        println!("network quota: {:?}", network.quota());

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_namespaces_1() {
    snippet_namespaces_1::main().expect("snippet failed");
}

mod snippet_namespaces_2 {
    use cachegirl::{NamespaceQuota, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-quota-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("scratch")?;

        ns.set_quota(NamespaceQuota::with_max_bytes(8 * 1024 * 1024)); // bytes only
        ns.set_quota(NamespaceQuota::with_max_resources(100)); // count only
        ns.set_quota(NamespaceQuota::with_limits(8 * 1024 * 1024, 100)); // both
        ns.set_quota(NamespaceQuota::unlimited()); // clear

        println!("usage: {:?}", ns.usage()?);
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_namespaces_2() {
    snippet_namespaces_2::main().expect("snippet failed");
}

mod snippet_namespaces_3 {
    use cachegirl::TempManager;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-usage-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("scratch")?;

        let file = ns.create_file("probe.bin")?;
        std::fs::write(file.path(), b"abc").expect("file is writable");
        ns.refresh_size(file.id())?;

        let (bytes, count) = ns.usage()?;
        println!("bytes={bytes} count={count}");
        assert_eq!(bytes, 3);

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_namespaces_3() {
    snippet_namespaces_3::main().expect("snippet failed");
}

mod snippet_observability_1 {
    use cachegirl::TempManager;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-obs-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let file = ns.create_file("probe.bin")?;
        let stats = manager.stats()?;
        println!("{}", stats.render());

        ns.release(file.id())?;
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_observability_1() {
    snippet_observability_1::main().expect("snippet failed");
}

mod snippet_observability_2 {
    use cachegirl::{Event, EventSink, TempManager, VecSink};
    use std::sync::Arc;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-sink-{}", std::process::id()));
        let sink = Arc::new(VecSink::new());
        let manager = TempManager::builder()
            .root(&dir)
            .event_sink(Arc::clone(&sink) as Arc<dyn EventSink>)
            .build()?;

        let ns = manager.namespace("demo")?;
        let file = ns.create_file("probe.bin")?;
        ns.release(file.id())?;

        for event in sink.drain() {
            match event {
                Event::ResourceCreated {
                    kind, namespace, ..
                } => {
                    println!("created kind={kind} namespace={namespace}")
                }
                Event::ResourceReleased { bytes_freed, .. } => {
                    println!("released bytes_freed={bytes_freed}")
                }
                other => println!("{other:?}"),
            }
        }

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_observability_2() {
    snippet_observability_2::main().expect("snippet failed");
}

mod snippet_observability_3 {
    use cachegirl::{TempManager, TracingSink};
    use std::sync::Arc;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-tracing-{}", std::process::id()));
        let manager = TempManager::builder()
            .root(&dir)
            .event_sink(Arc::new(TracingSink::new()) as Arc<dyn cachegirl::EventSink>)
            .build()?;

        let ns = manager.namespace("demo")?;
        let file = ns.create_file("probe.bin")?;
        ns.release(file.id())?;

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_observability_3() {
    snippet_observability_3::main().expect("snippet failed");
}

mod snippet_ownership_and_leases_1 {
    use cachegirl::{CreateOptions, Ownership, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-lease-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let file = ns.create_file_with(
            "leased.bin",
            CreateOptions::new().ownership(Ownership::Leased),
        )?;
        let path = file.path().to_path_buf();
        let id = file.id().clone();

        let lease = file.acquire()?;
        println!("release with lease held: {}", ns.release(&id)?);
        assert!(path.exists(), "a held lease must block deletion");

        drop(lease);
        println!("release after lease:     {}", ns.release(&id)?);
        assert!(!path.exists());

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_ownership_and_leases_1() {
    snippet_ownership_and_leases_1::main().expect("snippet failed");
}

mod snippet_ownership_and_leases_2 {
    use cachegirl::TempManager;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-stats-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let file = ns.create_file("hot.bin")?;
        let lease_a = file.acquire()?;
        let lease_b = file.acquire()?;
        println!("active leases: {}", manager.stats()?.active_leases);
        drop(lease_a);
        drop(lease_b);
        println!("active leases: {}", manager.stats()?.active_leases);

        ns.release(file.id())?;
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_ownership_and_leases_2() {
    snippet_ownership_and_leases_2::main().expect("snippet failed");
}

mod snippet_resources_1 {
    use cachegirl::CreateOptions;
    use std::time::Duration;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-res-{}", std::process::id()));
        let manager = cachegirl::TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        // filesystem resources (default features)
        let file = ns.create_file("packet.bin")?;
        let with_opts = ns.create_file_with(
            "packet-with-ttl.bin",
            CreateOptions::new().ttl(Duration::from_secs(60)),
        )?;
        let directory = ns.create_directory("staging")?;

        println!("file: {}", file.path().display());
        println!("dir:  {}", directory.path().display());
        println!("ttl:  {:?}", manager.registry().get(with_opts.id())?.ttl);

        ns.release(file.id())?;
        ns.release(with_opts.id())?;
        ns.release(directory.id())?;
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_resources_1() {
    snippet_resources_1::main().expect("snippet failed");
}

mod snippet_resources_2 {
    use cachegirl::CreateOptions;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-mem-{}", std::process::id()));
        let manager = cachegirl::TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let buffer = ns.create_buffer(64 * 1024)?;
        println!("buffer capacity: {}", buffer.capacity());

        let serialized = ns.create_serialization("frame-1", vec![1, 2, 3, 4])?;
        println!("serialization bytes: {}", serialized.len());

        let runtime = ns.create_runtime_with("req-42", CreateOptions::new().owner("billing"))?;
        println!("runtime marker: {}", runtime.name());

        let entry = ns.create_cache_entry("pkt-1", vec![0xde, 0xad, 0xbe, 0xef])?;
        let fetched = ns.cache_get("pkt-1")?.expect("entry must be cached");
        println!("cache hit: {} bytes", fetched.value.len());

        ns.release(buffer.id())?;
        ns.release(serialized.id())?;
        ns.release(runtime.id())?;
        ns.release(entry.id())?;
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_resources_2() {
    snippet_resources_2::main().expect("snippet failed");
}

mod snippet_resources_3 {
    use cachegirl::{CreateOptions, Ownership, Priority};
    use std::time::Duration;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-opts-{}", std::process::id()));
        let manager = cachegirl::TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let file = ns.create_file_with(
            "important.bin",
            CreateOptions::new()
                .owner("billing-worker")
                .ttl(Duration::from_secs(300))
                .ownership(Ownership::Managed)
                .priority(Priority::High)
                .metadata("request", "req-42"),
        )?;
        let meta = manager.registry().get(file.id())?;
        println!(
            "owner={:?} ttl={:?} priority={:?}",
            meta.owner, meta.ttl, meta.priority
        );
        println!(
            "request={}",
            meta.metadata
                .get("request")
                .map(String::as_str)
                .unwrap_or("?")
        );

        ns.release(file.id())?;
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_resources_3() {
    snippet_resources_3::main().expect("snippet failed");
}

mod snippet_resources_4 {
    use cachegirl::CreateOptions;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-cache-{}", std::process::id()));
        let manager = cachegirl::TempManager::builder().root(&dir).build()?;
        let ns = manager.namespace("demo")?;

        let entry = ns.create_cache_entry_with(
            "greeting",
            b"hello".to_vec(),
            CreateOptions::new().metadata("kind", "test"),
        )?;
        println!("entry id: {}", entry.id());

        if let Some(got) = ns.cache_get("greeting")? {
            println!("cache hit (created at {:?})", got.created_at);
        }

        ns.release(entry.id())?;
        assert!(
            ns.cache_get("greeting")?.is_none(),
            "released entries disappear"
        );
        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_resources_4() {
    snippet_resources_4::main().expect("snippet failed");
}

mod snippet_scheduler_integration_1 {
    use cachegirl::{CreateOptions, TempManager};
    use std::time::Duration;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-inline-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;

        let ns = manager.namespace("jobs")?;
        ns.create_file_with(
            "intermediate.bin",
            CreateOptions::new().ttl(Duration::from_millis(1)),
        )?;
        std::thread::sleep(Duration::from_millis(20));

        // "the application decided it is time" — one line, no extra machinery
        let report = manager.cleanup_expired()?;
        println!("inline sweep: removed={}", report.removed);

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_scheduler_integration_1() {
    snippet_scheduler_integration_1::main().expect("snippet failed");
}

mod snippet_scheduler_integration_2 {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use cachegirl::{CreateOptions, TempManager};

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-sched-{}", std::process::id()));
        let manager = Arc::new(
            TempManager::builder()
                .root(&dir)
                .cleanup_on_shutdown(true)
                .build()?,
        );

        let ns = manager.namespace("work")?;
        ns.create_file_with(
            "short-lived.bin",
            CreateOptions::new().ttl(Duration::from_secs(1)),
        )?;

        let stop = Arc::new(AtomicBool::new(false));
        let scheduler = {
            let manager = Arc::clone(&manager);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match manager.cleanup_expired() {
                        Ok(report) => {
                            if report.removed > 0 {
                                println!("sweep reclaimed {} resource(s)", report.removed);
                            }
                        }
                        Err(e) => eprintln!("sweep failed: {e}"),
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };

        std::thread::sleep(Duration::from_millis(1100));
        stop.store(true, Ordering::Relaxed);
        scheduler.join().expect("scheduler thread joins");

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_scheduler_integration_2() {
    snippet_scheduler_integration_2::main().expect("snippet failed");
}

mod snippet_scheduler_integration_3 {
    use cachegirl::AsyncTempManager;
    use cachegirl::CreateOptions;
    use std::time::Duration;

    #[tokio::main]
    pub async fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-tokio-{}", std::process::id()));
        let manager = AsyncTempManager::builder()
            .root(&dir)
            .cleanup_on_shutdown(true)
            .build()
            .await?;

        let ns = manager.namespace("work").await?;
        ns.create_file_with(
            "short-lived.bin",
            CreateOptions::new().ttl(Duration::from_secs(1)),
        )
        .await?;

        let mut interval = tokio::time::interval(Duration::from_millis(100));
        for _ in 0..12 {
            interval.tick().await;
            let report = manager.cleanup_expired().await?;
            if report.removed > 0 {
                println!("sweep reclaimed {} resource(s)", report.removed);
                break;
            }
        }

        manager.shutdown().await?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_scheduler_integration_3() {
    snippet_scheduler_integration_3::main().expect("snippet failed");
}

mod snippet_sync_and_async_1 {
    use cachegirl::AsyncTempManager;

    #[tokio::main]
    pub async fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-async-{}", std::process::id()));
        let manager = AsyncTempManager::builder()
            .root(&dir)
            .instance_id("example-instance")
            .cleanup_on_shutdown(true)
            .build()
            .await?;

        let protocol = manager.namespace("protocol").await?;

        let file = protocol.create_file("packet-dump.bin").await?;
        std::fs::write(file.path(), b"\x00\x01\x02\x03").expect("write packet dump");
        let size = protocol.refresh_size(file.id()).await?;
        println!("tracked {size} bytes");

        let file_id = file.id().clone();
        protocol.release(&file_id).await?;

        let report = manager.cleanup_expired().await?;
        println!(
            "cleanup: scanned={} removed={}",
            report.scanned, report.removed
        );

        manager.shutdown().await?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_sync_and_async_1() {
    snippet_sync_and_async_1::main().expect("snippet failed");
}

mod snippet_sync_and_async_2 {
    use cachegirl::AsyncTempManager;

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-norun-{}", std::process::id()));

        let future = async {
            let manager = AsyncTempManager::builder().root(&dir).build().await?;
            let ns = manager.namespace("protocol").await?;
            assert_eq!(ns.name(), "protocol");
            manager.shutdown().await?;
            Ok::<(), cachegirl::Error>(())
        };

        // any executor will do; this is the smallest one imaginable
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(result) => {
                    result?;
                    break;
                }
                std::task::Poll::Pending => std::thread::yield_now(),
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_sync_and_async_2() {
    snippet_sync_and_async_2::main().expect("snippet failed");
}

mod snippet_writing_a_backend_1 {
    use cachegirl::{
        CreateOptions, TempManager,
        backend::{ResourceBackend, ResourceRequest},
    };
    use std::sync::Arc;

    struct NullBackend;

    impl ResourceBackend for NullBackend {
        fn name(&self) -> &str {
            "null-backend"
        }
        fn create(&self, request: &ResourceRequest) -> cachegirl::Result<cachegirl::ResourceMeta> {
            Ok(cachegirl::ResourceMeta::new(
                &request.name,
                &request.namespace,
                request.kind,
            ))
        }
        fn release(&self, _meta: &cachegirl::ResourceMeta) -> cachegirl::Result<()> {
            Ok(())
        }
    }

    pub fn main() -> cachegirl::Result<()> {
        let dir = std::env::temp_dir().join(format!("cachegirl-backend-{}", std::process::id()));
        let manager = TempManager::builder().root(&dir).build()?;
        manager.register_backend(Arc::new(NullBackend))?;

        let ns = manager.namespace("demo")?;
        let res = ns.create_custom_with(
            "thing",
            "null-backend",
            CreateOptions::new().owner("example"),
        )?;
        println!("created via backend: {}", res.backend());
        println!("release -> {}", ns.release(res.id())?);

        manager.shutdown()?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[test]
fn snippet_writing_a_backend_1() {
    snippet_writing_a_backend_1::main().expect("snippet failed");
}
