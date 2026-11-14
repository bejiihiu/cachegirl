//! Wiring cachegirl into your own scheduler.
//!
//! The library deliberately owns no timer, no thread and no runtime: it
//! exposes `cleanup_expired()` and gets out of the way. This example runs
//! a plain `std::thread` + interval loop that calls it on a 100 ms cadence,
//! and stops the loop gracefully through an `AtomicBool`.
//!
//! Nothing about the pattern is special: a tokio interval, a cron job, a
//! work queue or a shutdown hook all work the same - call the sweep, read
//! the report, decide what to do with it.
//!
//! ```sh
//! cargo run --example custom_scheduler
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cachegirl::{CreateOptions, TempManager};

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-scheduler-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let manager = Arc::new(
        TempManager::builder()
            .root(&root)
            .create_if_missing(true)
            .instance_id("instance-scheduler")
            .cleanup_on_shutdown(true)
            .build()?,
    );

    // one resource with a short TTL so the scheduler has something to reclaim
    let work = manager.namespace("work")?;
    let file = work.create_file_with(
        "intermediate.bin",
        CreateOptions::new().ttl(Duration::from_secs(1)),
    )?;
    std::fs::write(file.path(), b"partial computation, safe to recompute")
        .expect("file is writable");
    work.refresh_size(file.id())?; // sync tracked size so the sweep can report bytes freed
    drop(file); // Managed handle: the file stays put until the sweep says otherwise

    // -- the scheduler itself lives entirely in this example ----------------
    let stop = Arc::new(AtomicBool::new(false));
    let scheduler = {
        let manager = Arc::clone(&manager);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut reclaimed = 0u64;
            let mut ticks = 0u64;
            while !stop.load(Ordering::Relaxed) {
                ticks += 1;
                match manager.cleanup_expired() {
                    Ok(report) => {
                        if report.removed > 0 {
                            println!(
                                "tick {ticks}: reclaimed {} resource(s), {} bytes freed",
                                report.removed, report.bytes_freed
                            );
                            reclaimed += report.removed;
                            break; // our one short-lived file is gone; done
                        }
                    }
                    Err(e) => eprintln!("tick {ticks}: cleanup pass failed: {e}"),
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            (reclaimed, ticks)
        })
    };

    // meanwhile the "application" does its own thing
    println!("application: doing real work while the scheduler ticks...");
    std::thread::sleep(Duration::from_millis(1100));

    // graceful stop: ask the loop to exit, then join it
    stop.store(true, Ordering::Relaxed);
    match scheduler.join() {
        Ok((reclaimed, ticks)) => {
            println!(
                "application: scheduler stopped gracefully after {ticks} tick(s), reclaimed {reclaimed} resource(s)"
            )
        }
        Err(_) => eprintln!("application: scheduler thread panicked"),
    }

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
