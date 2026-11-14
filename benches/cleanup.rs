//! Cleanup pass cost against a pre-filled expired registry.
//!
//! Each round starts from scratch: N fresh managed buffers with a TTL
//! that is already in the past (backdated through the registry, the
//! same trick the integration tests use — no sleeps), then the timed
//! part is exactly one `cleanup_expired()` call.

#[path = "common/mod.rs"]
mod common;

#[allow(unused_imports)]
use common::{BenchConfig, TempRoot, bench_setup, print_platform};

#[cfg(feature = "memory")]
fn run_tier(root: &std::path::Path, n: usize, warmup: usize, rounds: usize) {
    use std::time::{Duration, SystemTime};

    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id(format!("bench-cleanup-{n}"))
        .build()
        .expect("manager builds");
    let ns = manager.namespace("bench").expect("namespace");
    let registry = manager.registry();

    let cfg_name = match n {
        100 => "cleanup/expired_100",
        1_000 => "cleanup/expired_1000",
        _ => "cleanup/expired_10000",
    };
    let cfg = BenchConfig::new(cfg_name, n, warmup, rounds);

    bench_setup(
        &cfg,
        || {
            // fill the registry with resources whose TTL is already past
            let past = SystemTime::now() - Duration::from_secs(7200);
            let ttl = cachegirl::CreateOptions::new().ttl(Duration::from_secs(3600));
            let mut ids = Vec::with_capacity(n);
            for _ in 0..n {
                let buf = ns
                    .create_buffer_with(128, ttl.clone())
                    .expect("create expired candidate");
                ids.push(buf.id().clone());
            }
            for id in &ids {
                registry
                    .update(id, |m| {
                        m.created_at = past;
                        m.last_accessed = past;
                    })
                    .expect("backdate");
            }
        },
        |_| {
            let report = manager.cleanup_expired().expect("cleanup pass");
            assert_eq!(
                report.removed, n as u64,
                "every backdated resource must be reclaimed"
            );
        },
    );

    manager.shutdown().expect("clean shutdown");
}

fn main() {
    print_platform();
    #[allow(unused_variables)]
    let root = TempRoot::new("cleanup");

    #[cfg(feature = "memory")]
    {
        run_tier(root.path(), 100, 2, 10);
        run_tier(root.path(), 1_000, 2, 7);
        run_tier(root.path(), 10_000, 1, 5);
    }
    #[cfg(not(feature = "memory"))]
    println!("skip cleanup benches: enable the `memory` feature");
}
