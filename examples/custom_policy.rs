//! A cleanup policy of your own, in about ten lines.
//!
//! `CustomCleanupPolicy` is consulted for resources that no built-in rule
//! selected in a pass: TTL didn't lapse, no quota pushed, nothing else
//! nominated them. It answers one question per resource - keep or
//! reclaim - and a `Remove` verdict reclaims it in the same pass.
//! Leases still win: a `Remove` verdict on a leased resource defers
//! destruction instead of forcing it.
//!
//! The policy here is a "keep tagged" janitor: anything whose metadata
//! says `keep=true` is left alone, everything else is reclaimed. This is
//! the closure form - closures with the right signature implement the
//! trait for free - but an ordinary struct implementing the trait works
//! the same.
//!
//! ```sh
//! cargo run --example custom_policy
//! ```

use std::sync::Arc;

use cachegirl::{CleanupContext, CreateOptions, PolicyDecision, ResourceMeta, TempManager};

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-policy-{:x}",
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

    let ns = manager.namespace("scratch")?;
    let precious = ns.create_file_with(
        "precious.bin",
        CreateOptions::new().metadata("keep", "true"),
    )?;
    let temp_a = ns.create_file("temp-a.bin")?;
    let temp_b = ns.create_file("temp-b.bin")?;
    let precious_path = precious.path().to_path_buf();
    let temp_a_path = temp_a.path().to_path_buf();
    let temp_b_path = temp_b.path().to_path_buf();
    println!("created precious.bin (keep=true), temp-a.bin, temp-b.bin");

    // keep what is tagged, reclaim what is not
    manager.set_custom_cleanup_policy(Arc::new(|meta: &ResourceMeta, _ctx: &CleanupContext| {
        match meta.metadata.get("keep").map(String::as_str) {
            Some("true") => PolicyDecision::Keep,
            _ => PolicyDecision::Remove,
        }
    }));

    let report = manager.cleanup_expired()?;
    println!(
        "policy sweep: scanned {}, removed {} (policy={})",
        report.scanned, report.removed, report.reasons.policy
    );
    println!("precious.bin still on disk: {}", precious_path.exists());
    println!("temp-a.bin on disk:   {}", temp_a_path.exists());
    println!("temp-b.bin on disk:   {}", temp_b_path.exists());
    assert!(precious_path.exists(), "keep=true must survive the sweep");
    assert!(!temp_a_path.exists(), "untagged resources are reclaimed");
    assert!(!temp_b_path.exists(), "untagged resources are reclaimed");

    // take the policy off again; precious can be released explicitly
    manager.clear_custom_cleanup_policy();
    ns.release(precious.id())?;
    println!("precious.bin released explicitly");

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
