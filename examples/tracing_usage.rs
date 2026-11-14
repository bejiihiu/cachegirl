//! Run cachegirl with tracing output on stdout.
//!
//! Shows how the manager, namespaces, resources and the cleanup engine
//! report through spans and events. Add your own subscriber to see
//! everything, or filter levels with your usual tracing setup.
//!
//! ```sh
//! cargo run --features tracing --example tracing_usage
//! ```

use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let dir = std::env::temp_dir().join(format!("cachegirl-tracing-usage-{}", std::process::id()));
    let manager = TempManager::builder()
        .root(&dir)
        .instance_id("instance-tracing-usage")
        .cleanup_on_shutdown(true)
        .build()?;

    let namespace = manager.namespace("demo")?;
    let file = namespace.create_file("trace-me.bin")?;
    let id = file.id().clone();

    // leases, releases and cleanup passes all show up as events/spans
    let lease = file.acquire()?;
    drop(lease);
    namespace.release(&id)?;

    manager.cleanup_expired()?;
    manager.shutdown()?;

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
