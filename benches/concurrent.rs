//! Concurrent create/release across a shared namespace.
//!
//! Eight workers, 250 create+release pairs each = 2_000 ops per round.
//! One barrier lines all workers up at round start, and the round is
//! timed wall-clock from first start to last finish — what an 8-core
//! machine actually feels, contention included (the registry serializes
//! every create, which is exactly what this bench puts pressure on).

#[path = "common/mod.rs"]
mod common;

#[allow(unused_imports)]
use common::{BenchConfig, TempRoot, bench, print_platform};

#[cfg(feature = "memory")]
fn bench_concurrent_buffers(root: &std::path::Path) {
    use std::sync::{Arc, Barrier};

    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-concurrent")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("hot").expect("namespace");

    const WORKERS: usize = 8;
    const OPS_PER_WORKER: usize = 250;

    // 1 warmup round + 10 measured rounds, 2_000 ops each
    let cfg = BenchConfig::new("concurrent/8x250_buffer_create_release", 2_000, 1, 10);
    bench(&cfg, || {
        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut handles = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let ns = Arc::clone(&ns);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..OPS_PER_WORKER {
                    let buf = ns.create_buffer(256).expect("create buffer");
                    ns.release(buf.id()).expect("release");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker must not panic");
        }
    });

    manager.shutdown().expect("clean shutdown");
}

fn main() {
    print_platform();
    #[allow(unused_variables)]
    let root = TempRoot::new("concurrent");

    #[cfg(feature = "memory")]
    bench_concurrent_buffers(root.path());
    #[cfg(not(feature = "memory"))]
    println!("skip concurrent benches: enable the `memory` feature");
}
