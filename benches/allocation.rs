//! Allocation paths: what it costs to open and close a resource.
//!
//! Buffer create+release, the `Owned` drop path for comparison, file
//! create+release on a real filesystem, and a packet-style 64 KiB
//! create/use/release cycle.

#[path = "common/mod.rs"]
mod common;

#[allow(unused_imports)]
use common::{BenchConfig, TempRoot, bench, observed, print_platform};

#[cfg(feature = "memory")]
fn bench_buffer_create_release(root: &std::path::Path) {
    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-alloc-buffer")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("bench").expect("namespace");

    // 500 ops x 20 rounds = 10_000 measured create+release pairs
    let cfg = BenchConfig::new("allocation/buffer_create_release", 500, 4, 20);
    bench(&cfg, || {
        for _ in 0..500 {
            let buf = observed(ns.create_buffer(4096).expect("create buffer"));
            observed(ns.release(buf.id()).expect("release"));
        }
    });
    manager.shutdown().expect("clean shutdown");
}

#[cfg(feature = "memory")]
fn bench_buffer_drop_owned(root: &std::path::Path) {
    use cachegirl::Ownership;

    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-alloc-owned")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("bench").expect("namespace");

    // same shape as the release path on purpose: the delta between the
    // two numbers is what the Drop glue costs on top of destroy_resource
    let cfg = BenchConfig::new("allocation/buffer_drop_owned", 500, 4, 20);
    bench(&cfg, || {
        for _ in 0..500 {
            let buf = ns
                .create_buffer_with(
                    4096,
                    cachegirl::CreateOptions::new().ownership(Ownership::Owned),
                )
                .expect("create owned buffer");
            drop(observed(buf)); // last drop runs the destroy path
        }
    });
    manager.shutdown().expect("clean shutdown");
}

#[cfg(feature = "filesystem")]
fn bench_file_create_release(root: &std::path::Path) {
    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-alloc-file")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("bench").expect("namespace");
    let mut seq = 0u64;

    // 100 files per round, 10 rounds = 1_000 measured create+release pairs
    let cfg = BenchConfig::new("allocation/file_create_release", 100, 2, 10);
    bench(&cfg, || {
        for _ in 0..100 {
            seq += 1;
            let file = observed(
                ns.create_file(&format!("bench-{seq}.bin"))
                    .expect("create file"),
            );
            observed(ns.release(file.id()).expect("release"));
        }
    });
    manager.shutdown().expect("clean shutdown");
}

#[cfg(feature = "memory")]
fn bench_packet_workload(root: &std::path::Path) {
    const PACKET: usize = 64 * 1024;

    let manager = cachegirl::TempManager::builder()
        .root(root)
        .instance_id("bench-alloc-packet")
        .build()
        .expect("manager builds");
    let ns = manager.namespace("packets").expect("namespace");

    // 200 packets per round, 50 rounds = 10_000 measured packet cycles.
    // one cycle = allocate, write 64 KiB, read it back, release — the
    // shape of a packet-processing hot path, not a pure alloc microbench
    let cfg = BenchConfig::new("packet/buffer_64kb_create_use_release", 200, 5, 50);
    bench(&cfg, || {
        for i in 0..200u64 {
            let mut buf = ns.create_buffer(PACKET).expect("create packet buffer");
            {
                let bytes = buf.as_mut_slice();
                // cheap deterministic fill, one byte of pattern per byte
                for (j, b) in bytes.iter_mut().enumerate() {
                    *b = (j as u64 ^ i).wrapping_mul(2654435761) as u8;
                }
            }
            let checksum = buf
                .as_slice()
                .iter()
                .fold(0u64, |acc, &b| acc.wrapping_add(b as u64).rotate_left(5));
            observed(checksum);
            observed(ns.release(buf.id()).expect("release"));
        }
    });
    manager.shutdown().expect("clean shutdown");
}

fn main() {
    print_platform();
    #[allow(unused_variables)]
    let root = TempRoot::new("allocation");

    #[cfg(feature = "memory")]
    {
        bench_buffer_create_release(root.path());
        bench_buffer_drop_owned(root.path());
    }
    #[cfg(not(feature = "memory"))]
    println!("skip allocation/buffer_* benches: enable the `memory` feature");

    #[cfg(feature = "filesystem")]
    bench_file_create_release(root.path());
    #[cfg(not(feature = "filesystem"))]
    println!("skip allocation/file_create_release: enable the `filesystem` feature");

    #[cfg(feature = "memory")]
    bench_packet_workload(root.path());
    #[cfg(not(feature = "memory"))]
    println!("skip packet/buffer_64kb_create_use_release: enable the `memory` feature");
}
