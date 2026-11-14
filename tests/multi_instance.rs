//! End-to-end behavior of multiple managers over a shared root.
//!
//! No global state, per-instance lock files, and crash recovery:
//! one manager must not be able to see — let alone reclaim —
//! another's resources, and a crashed instance gets reaped at startup
//! while live ones are left strictly alone.

#![cfg(feature = "filesystem")]

mod common;

use std::time::{Duration, SystemTime};

use cachegirl::{CreateOptions, TempManager};
use common::{TestDir, backdate};

#[test]
fn two_managers_on_one_root_stay_isolated() {
    let root = TestDir::new("two-managers");
    let manager_a = TempManager::builder()
        .root(root.path())
        .instance_id("instance-alpha")
        .build()
        .unwrap();
    let manager_b = TempManager::builder()
        .root(root.path())
        .instance_id("instance-beta")
        .build()
        .unwrap();

    // same namespace, same file name: they still land in different
    // instance directories and never share a record
    let ns_a = manager_a.namespace("work").unwrap();
    let ns_b = manager_b.namespace("work").unwrap();
    let file_a = ns_a
        .create_file_with(
            "shared-name.txt",
            CreateOptions::new().ttl(Duration::from_secs(3600)),
        )
        .unwrap();
    let file_b = ns_b.create_file("shared-name.txt").unwrap();

    // the manager canonicalizes its root, so compare against what it reports
    assert_ne!(file_a.path(), file_b.path());
    assert!(
        file_a
            .path()
            .starts_with(manager_a.root().join("instance-alpha"))
    );
    assert!(
        file_b
            .path()
            .starts_with(manager_b.root().join("instance-beta"))
    );

    assert_eq!(manager_a.registry().len().unwrap(), 1);
    assert_eq!(manager_b.registry().len().unwrap(), 1);
    assert!(manager_a.registry().get(file_b.id()).is_err());
    assert!(manager_b.registry().get(file_a.id()).is_err());

    // a clean shutdown of one removes its lock and nothing of the other's
    let report = manager_b.shutdown().unwrap();
    assert_eq!(report.removed, 0);
    assert!(file_a.path().exists());
    assert!(file_b.path().exists());
    assert!(
        !manager_b
            .root()
            .join("instance-beta")
            .join(".cachegirl.lock")
            .exists()
    );
    assert!(
        manager_a
            .root()
            .join("instance-alpha")
            .join(".cachegirl.lock")
            .exists()
    );

    // sweeping an expiry in A leaves B's file alone
    backdate(&manager_a, file_a.id(), 7200);
    let report = manager_a.cleanup_expired().unwrap();
    assert_eq!(report.removed, 1);
    assert!(!file_a.path().exists());
    assert!(file_b.path().exists());

    // even a startup recovery pass reclaims nothing that is still alive
    let manager_c = TempManager::builder()
        .root(root.path())
        .instance_id("instance-gamma")
        .cleanup_on_start(true)
        .build()
        .unwrap();
    let recovery = manager_c.recovery_report().unwrap();
    assert_eq!(recovery.removed, 0, "{recovery}");
    assert_eq!(
        recovery.kept, 1,
        "the live instance must be kept: {recovery}"
    );
    assert!(file_b.path().exists());
    manager_c.shutdown().unwrap();
    manager_a.shutdown().unwrap();
}

#[test]
fn crash_simulation_reclaims_the_dead_instance_only() {
    let root = TestDir::new("crash");

    // a healthy instance: pid is this process, token matches, dir survives
    let live = TempManager::builder()
        .root(root.path())
        .instance_id("instance-live")
        .build()
        .unwrap();
    let ns = live.namespace("keep").unwrap();
    let precious = ns.create_file("precious.bin").unwrap();

    // a genuinely dead process: spawn it, reap it, use its pid
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "exit", "0"])
        .spawn()
        .unwrap();
    #[cfg(unix)]
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();

    // the crash left an instance directory with its lock and data behind
    let dead_dir = root.path().join("instance-dead");
    std::fs::create_dir_all(dead_dir.join("keep")).unwrap();
    std::fs::write(dead_dir.join("keep").join("leftover.bin"), vec![b'x'; 4096]).unwrap();
    let lock = dead_dir.join(".cachegirl.lock");
    std::fs::write(
        &lock,
        format!(
            "# cachegirl instance lock — do not edit\nversion=2\npid={dead_pid}\ncreated=1763100000\ninstance=instance-dead\nstart=crashed-lifetime-token\n"
        ),
    )
    .unwrap();
    // age the lock file the way a long-forgotten crash would look
    let ancient = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(ancient)
        .unwrap();

    // a fresh manager with cleanup_on_start reaps the wreckage at startup
    let manager = TempManager::builder()
        .root(root.path())
        .instance_id("instance-restart")
        .cleanup_on_start(true)
        .build()
        .unwrap();
    let report = manager.recovery_report().unwrap();
    assert_eq!(report.removed, 1, "{report}");
    assert!(report.bytes_freed >= 4096);
    assert!(!dead_dir.exists());

    // the live instance is not part of the wreckage
    assert!(precious.path().exists());
    assert!(live.registry().get(precious.id()).is_ok());

    manager.shutdown().unwrap();
    live.shutdown().unwrap();
}

#[test]
fn lockless_ghost_directory_is_kept_until_it_is_old_enough() {
    let root = TestDir::new("lockless");
    let ghost = root.path().join("instance-ghost");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("data.bin"), b"leftover").unwrap();

    // fresh mtime, no lock: the default one-hour threshold keeps it
    let manager = TempManager::builder()
        .root(root.path())
        .instance_id("instance-first")
        .cleanup_on_start(true)
        .build()
        .unwrap();
    let report = manager.recovery_report().unwrap();
    assert_eq!(report.removed, 0, "{report}");
    assert!(ghost.exists());
    manager.shutdown().unwrap();

    // with a zero threshold the same directory counts as stale
    let manager = TempManager::builder()
        .root(root.path())
        .instance_id("instance-second")
        .cleanup_on_start(true)
        .stale_instance_after(Duration::ZERO)
        .build()
        .unwrap();
    let report = manager.recovery_report().unwrap();
    assert!(report.removed >= 1, "{report}");
    assert!(!ghost.exists());
    manager.shutdown().unwrap();
}
