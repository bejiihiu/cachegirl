//! Cache churn: create → get → release over a 1k-key working set.
//!
//! The same keys are recycled round after round, which is the intended
//! steady state of a cached pipeline (insert → hit → evict, repeat).
//! Each round is one full sweep of the keyspace; one op is one
//! triple, so the numbers are per key-lifecycle, not per call.

#[path = "common/mod.rs"]
mod common;

#[allow(unused_imports)]
use common::{BenchConfig, TempRoot, bench, print_platform};

#[cfg(feature = "cache")]
fn bench_cache_churn(root: &std::path::Path) {
    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-cache")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("bench").expect("namespace");

    const KEYS: usize = 1_000;
    let payload = vec![0xABu8; 1024]; // 1 KiB values, like small serialized frames

    let cfg = BenchConfig::new("cache/churn_1000_keys", KEYS, 2, 10);
    bench(&cfg, || {
        for i in 0..KEYS {
            let key = format!("key-{i:04}");
            let entry = ns
                .create_cache_entry(&key, payload.clone())
                .expect("create entry");
            let hit = ns.cache_get(&key).expect("get entry");
            assert!(
                hit.is_some(),
                "entry must be retrievable right after create"
            );
            ns.release(entry.id()).expect("release entry");
        }
    });

    manager.shutdown().expect("clean shutdown");
}

fn main() {
    print_platform();
    #[allow(unused_variables)]
    let root = TempRoot::new("cache");

    #[cfg(feature = "cache")]
    bench_cache_churn(root.path());
    #[cfg(not(feature = "cache"))]
    println!("skip cache benches: enable the `cache` feature");
}
