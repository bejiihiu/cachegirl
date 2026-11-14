//! Registry-only micro: register → get → remove, nothing else.
//!
//! The registry is the hot path every resource op passes through, so
//! its floor matters. One op is one full triple on a fresh record;
//! no backends, no quotas, no I/O.

#[path = "common/mod.rs"]
mod common;

use common::{BenchConfig, bench, print_platform};

fn bench_registry_register_get_remove() {
    use cachegirl::{Registry, ResourceKind, ResourceMeta};

    let registry = Registry::new();

    const OPS: usize = 1_000;
    // 1_000 triples x 20 rounds = 20_000 measured register+get+remove
    let cfg = BenchConfig::new("registry/register_get_remove", OPS, 5, 20);
    bench(&cfg, || {
        for _ in 0..OPS {
            let meta = ResourceMeta::new("bench", "bench", ResourceKind::Buffer);
            let id = meta.id.clone();
            registry.register(meta).expect("register");
            let got = registry.get(&id).expect("get");
            let removed = registry.remove(&got.id).expect("remove");
            common::observed(removed.id);
        }
    });
}

fn main() {
    print_platform();
    bench_registry_register_get_remove();
}
