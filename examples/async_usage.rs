//! Async usage of `cachegirl` on a Tokio runtime.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example async_usage --features tokio
//! ```
//!
//! Every operation that may block (filesystem access, cleanup sweeps,
//! startup recovery) goes to Tokio's blocking pool; memory-only
//! operations stay on the current thread.

use cachegirl::{AsyncTempManager, Ownership};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), cachegirl::Error> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-async-example-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let manager = AsyncTempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .instance_id("example-instance")
        .cleanup_on_shutdown(true)
        .build()
        .await?;

    let protocol = manager.namespace("protocol").await?;

    // filesystem resource: created on the blocking pool under Tokio
    let file = protocol.create_file("packet-dump.bin").await?;
    std::fs::write(file.path(), b"\x00\x01\x02\x03").expect("write packet dump");
    let size = protocol.refresh_size(file.id()).await?;
    println!("packet-dump.bin: {size} bytes tracked");

    // memory resource: created inline, no filesystem involved
    #[cfg(feature = "memory")]
    {
        let mut buffer = protocol.create_buffer(4096).await?;
        buffer.extend_from_slice(b"payload");
        println!("buffer: {} bytes registered", buffer.len());
    }

    // named workers hammer the same namespace concurrently; the
    // manager's quotas and registry keep them honest
    let mut tasks = Vec::new();
    for worker in 0..4 {
        let ns = protocol.clone();
        tasks.push(tokio::spawn(async move {
            let file = ns
                .create_file_with(
                    &format!("worker-{worker}.bin"),
                    cachegirl::CreateOptions::new().ownership(Ownership::Managed),
                )
                .await?;
            let id = file.id().clone();
            let released = ns.release(&id).await?;
            Ok::<_, cachegirl::Error>((worker, released))
        }));
    }
    for task in tasks {
        let (worker, released) = task.await.expect("worker task panicked")?;
        println!("worker {worker}: released cleanly = {released}");
    }

    let report = manager.cleanup_expired().await?;
    let stats = manager.stats().await?;
    println!(
        "cleanup: scanned {}, removed {}; resources tracked: {}, bytes tracked: {}",
        report.scanned, report.removed, stats.resources, stats.bytes_tracked
    );

    // explicit release first, then shutdown
    let file_id = file.id().clone();
    protocol.release(&file_id).await?;
    manager.shutdown().await?;

    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
