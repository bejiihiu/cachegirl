//! The four ownership modes, and what actually happens to the resource
//! in each one.
//!
//! - `Owned`   — last owning handle out, resource destroyed
//! - `Shared`  — co-owners like `Arc`; last one out destroys
//! - `Managed` — handle drops are free; lifetime is policy (TTL/release/sweep)
//! - `Leased`  — nothing deletes it while a lease is held
//!
//! ```sh
//! cargo run --example ownership
//! ```

use std::time::Duration;

use cachegirl::{CreateOptions, Ownership, TempManager};

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-ownership-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let manager = TempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .cleanup_on_shutdown(true)
        .build()?;

    let ns = manager.namespace("demo")?;

    // -- owned: drop destroys ----------------------------------------------
    let owned = ns.create_file_with(
        "owned.bin",
        CreateOptions::new().ownership(Ownership::Owned),
    )?;
    let owned_path = owned.path().to_path_buf();
    println!("owned:   exists after creation -> {}", owned_path.exists());
    drop(owned);
    println!("owned:   exists after drop    -> {}", owned_path.exists());

    // -- shared: two components, one resource --------------------------------
    let producer = ns.create_file_with(
        "shared.bin",
        CreateOptions::new().ownership(Ownership::Shared),
    )?;
    std::fs::write(producer.path(), b"shared payload").expect("file is writable");
    let shared_path = producer.path().to_path_buf();
    let shared_id = producer.id().clone();
    let consumer = producer.clone(); // component B gets its own co-owning handle

    drop(consumer);
    println!(
        "shared:  one co-owner dropped, file exists -> {}",
        shared_path.exists()
    );
    println!(
        "shared:  one co-owner dropped, record kept -> {}",
        manager.registry().get(&shared_id).is_ok()
    );
    drop(producer);
    println!(
        "shared:  last co-owner dropped, file exists -> {}",
        shared_path.exists()
    );

    // -- managed: policy decides, not the handle ----------------------------
    let managed = ns.create_file_with(
        "managed.bin",
        CreateOptions::new().ownership(Ownership::Managed),
    )?;
    let managed_path = managed.path().to_path_buf();
    let managed_id = managed.id().clone();
    drop(managed);
    println!("managed: exists after drop    -> {}", managed_path.exists());
    ns.release(&managed_id)?;
    println!("managed: exists after release -> {}", managed_path.exists());

    // managed + TTL: expires on its own, sweep does the deleting
    let timed = ns.create_file_with(
        "managed-ttl.bin",
        CreateOptions::new()
            .ownership(Ownership::Managed)
            .ttl(Duration::from_millis(150)),
    )?;
    let timed_path = timed.path().to_path_buf();
    std::thread::sleep(Duration::from_millis(300));
    let report = manager.cleanup_expired()?;
    println!(
        "managed: ttl lapse swept {} resource(s) (ttl.fixed={}), file exists -> {}",
        report.removed,
        report.reasons.ttl_fixed,
        timed_path.exists()
    );

    // -- leased: the lease is the gate ---------------------------------------
    let leased = ns.create_file_with(
        "leased.bin",
        CreateOptions::new().ownership(Ownership::Leased),
    )?;
    let leased_path = leased.path().to_path_buf();
    let leased_id = leased.id().clone();

    let lease = leased.acquire()?;
    // while the lease is held, release must refuse: parked, not deleted
    let outcome = ns.release(&leased_id)?;
    println!(
        "leased:  release while leased  -> {outcome} (file exists -> {})",
        leased_path.exists()
    );
    assert!(!outcome, "a held lease must defer the release");

    drop(lease);
    let outcome = ns.release(&leased_id)?;
    println!(
        "leased:  release after lease   -> {outcome} (file exists -> {})",
        leased_path.exists()
    );
    assert!(outcome, "with the lease gone, the release goes through");

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
