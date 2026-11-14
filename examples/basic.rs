//! The smallest complete `cachegirl` program, start to finish.
//!
//! One manager, one namespace, a file, a directory and (with the
//! `memory` feature) a buffer. A short-lived resource expires
//! under TTL, a cleanup pass reclaims it, the rest is shut down
//! cleanly.
//!
//! ```sh
//! cargo run --example basic
//! cargo run --all-features --example basic   # also shows the buffer section
//! ```

use std::time::Duration;

use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-basic-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    // the library never picks a location for you; this one is scratch space
    // for this run only, and it is removed when the example is done
    let manager = TempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .cleanup_on_shutdown(true)
        .build()?;
    println!(
        "manager up as instance `{}` (root: {})",
        manager.instance_id(),
        root.display()
    );

    let scratch = manager.namespace("scratch")?;

    // -- a file resource -------------------------------------------------
    // writes go to the path the handle gives you; the manager tracks size
    let file = scratch.create_file("packet-dump.bin")?;
    std::fs::write(file.path(), b"hello cachegirl").expect("writable by construction");
    let file_id = file.id().clone();
    let size = scratch.refresh_size(&file_id)?;
    println!("packet-dump.bin: {size} bytes tracked");

    // -- a directory resource --------------------------------------------
    let dir = scratch.create_directory("staging")?;
    std::fs::write(dir.path().join("nested.bin"), b"nested payload")
        .expect("directory is writable");
    println!("staging dir: {}", dir.path().display());

    // -- an in-memory buffer (cargo feature `memory`) --------------------
    #[cfg(feature = "memory")]
    let buffer_id = {
        let mut buffer = scratch.create_buffer(64 * 1024)?;
        buffer.extend_from_slice(b"packet payload");
        println!(
            "buffer: {} bytes written (capacity {})",
            buffer.len(),
            buffer.capacity()
        );
        buffer.id().clone()
    };
    #[cfg(not(feature = "memory"))]
    println!("buffer section skipped (build with --features memory to see it)");

    // -- a resource that expires on its own -------------------------------
    // fixed TTL counted from creation; the next cleanup pass reaps it
    let expiring = scratch.create_file_with(
        "short-lived.bin",
        cachegirl::CreateOptions::new().ttl(Duration::from_millis(150)),
    )?;
    std::fs::write(expiring.path(), b"scratch space, safe to recompute").expect("file is writable");
    let expiring_id = expiring.id().clone();
    scratch.refresh_size(&expiring_id)?;
    let expiring_path = expiring.path().to_path_buf();
    std::thread::sleep(Duration::from_millis(300)); // let the TTL actually lapse

    let report = manager.cleanup_expired()?;
    println!(
        "cleanup pass: scanned {}, removed {} (ttl.fixed={}), bytes freed {}",
        report.scanned, report.removed, report.reasons.ttl_fixed, report.bytes_freed
    );
    assert!(!expiring_path.exists(), "the TTL must have claimed it");

    // -- stats, then an orderly shutdown ----------------------------------
    let stats = manager.stats()?;
    println!(
        "stats: {} namespace(s), {} resource(s) tracked, {} bytes tracked",
        stats.namespaces, stats.resources, stats.bytes_tracked
    );

    // explicit release for what we still hold; the directory's nested
    // file goes with it
    scratch.release(&file_id)?;
    scratch.release(dir.id())?;
    #[cfg(feature = "memory")]
    scratch.release(&buffer_id)?;

    let shutdown = manager.shutdown()?;
    println!(
        "shutdown sweep: scanned {}, removed {}",
        shutdown.scanned, shutdown.removed
    );

    let _ = std::fs::remove_dir_all(&root);
    println!("done: everything the manager created has been reclaimed");
    Ok(())
}
