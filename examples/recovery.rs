//! Crash recovery, rehearsed on a real crash.
//!
//! This example re-executes itself as a child process. The child builds a
//! manager, creates an instance with one file in it, then dies abruptly
//! via `process::exit(1)` - no `shutdown()`, no lock-file removal, exactly
//! like an unclean termination. The parent then runs recovery the way an
//! application would on the next startup:
//!
//! 1. `ReportOnly` - classify everything, delete nothing
//! 2. `OnStartup` - reclaim what is certainly dead
//!
//! The parent's own instance directory is never touched.
//!
//! ```sh
//! cargo run --example recovery
//! ```

use std::path::{Path, PathBuf};

use cachegirl::{RecoveryAction, RecoveryManager, RecoveryPolicy, TempManager};

/// What the "crashing application" does. Never returns.
fn crashed_child(root: &Path) -> ! {
    let manager = TempManager::builder()
        .root(root)
        .instance_id("instance-crashed")
        .build()
        .expect("child: manager must build");
    let ns = manager.namespace("jobs").expect("child: namespace");
    let file = ns.create_file("inflight.log").expect("child: file");
    std::fs::write(file.path(), b"partial line, the process died here").expect("child: write");
    println!(
        "child: wrote inflight.log as pid {}; dying abruptly",
        std::process::id()
    );
    std::process::exit(1); // no Drop, no shutdown, no lock cleanup
}

fn action_name(action: RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::Removed => "removed",
        RecoveryAction::Kept => "kept",
        RecoveryAction::Skipped => "skipped",
        RecoveryAction::Failed => "failed",
    }
}

fn main() -> cachegirl::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--crashed-child") {
        let root = PathBuf::from(&args[2]);
        crashed_child(&root);
    }

    let root = std::env::temp_dir().join(format!(
        "cachegirl-recovery-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    // a healthy instance we control; it must survive every pass below
    let manager = TempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .instance_id("instance-current")
        .cleanup_on_shutdown(true)
        .build()?;
    println!(
        "parent: running as instance-current (pid {})",
        std::process::id()
    );

    // spawn the doomed child and wait for it to die
    let exe = std::env::current_exe().expect("current exe path is known");
    let status = std::process::Command::new(exe)
        .arg("--crashed-child")
        .arg(&root)
        .status()
        .expect("child process spawns");
    println!("parent: child exited with {status}");
    assert!(!status.success(), "the child is supposed to die badly");

    let crashed_dir = root.join("instance-crashed");
    println!(
        "parent: crashed instance dir still on disk -> {}",
        crashed_dir.exists()
    );
    if let Some(age) = cachegirl::recovery::lock_age(&crashed_dir) {
        println!("parent: its lock file is {age:?} old");
    }

    let recovery = RecoveryManager::new(&root);

    // -- pass 1: diagnose only, touch nothing -------------------------------
    println!("\nreport-only pass (classifies, deletes nothing):");
    let report = recovery.recover(RecoveryPolicy::ReportOnly, "instance-current")?;
    println!("{report}");
    for entry in &report.entries {
        println!(
            "  {} -> {}: {}",
            entry.instance,
            action_name(entry.action),
            entry.reason
        );
    }
    assert!(crashed_dir.exists(), "report-only must not delete anything");

    // -- pass 2: reclaim what is certainly dead ------------------------------
    println!("\non-startup pass (reclaims stale instances):");
    let report = recovery.recover(RecoveryPolicy::OnStartup, "instance-current")?;
    println!("{report}");
    for entry in &report.entries {
        println!(
            "  {} -> {}: {}",
            entry.instance,
            action_name(entry.action),
            entry.reason
        );
    }
    assert!(
        !crashed_dir.exists(),
        "the crashed instance must be reclaimed"
    );
    assert!(
        root.join("instance-current").exists(),
        "the live instance must survive"
    );

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
