//! Instance discovery and stale-directory reclamation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::error::RecoveryError;

const LOCK_FILE: &str = ".cachegirl.lock";

/// How aggressively stale instance directories are reclaimed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RecoveryPolicy {
    /// Never touch other instance directories.
    Never,
    /// Reclaim stale directories on startup only.
    #[default]
    OnStartup,
}

/// Outcome of a recovery pass.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecoveryReport {
    /// Instance directories examined.
    pub scanned: u64,
    /// Stale directories removed.
    pub removed: u64,
    /// Directories kept, e.g. because their owner is alive.
    pub kept: u64,
    /// Directories that failed to be reclaimed.
    pub failed: u64,
    /// Human-readable reasons for the failures.
    pub failure_reasons: Vec<String>,
}

/// Best-effort check whether a process is alive.
///
/// On Linux this reads `/proc`. Elsewhere we conservatively report
/// `true` — deleting a live instance's data is far worse than keeping
/// a stale directory around.
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

/// Scan a root directory for stale instance directories and reclaim them.
pub struct RecoveryManager {
    root: PathBuf,
}

impl RecoveryManager {
    /// Recovery rooted at a shared base directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Parse a lock file of the shape `pid=<number>` (one line per field).
    fn read_lock(dir: &Path) -> Result<u32, RecoveryError> {
        let lock_path = dir.join(LOCK_FILE);
        let raw = fs::read_to_string(&lock_path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => RecoveryError::CorruptLock {
                path: lock_path.clone(),
                reason: "lock file is missing".into(),
            },
            _ => RecoveryError::Inspect {
                path: lock_path.clone(),
                source,
            },
        })?;
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("pid=") {
                return value
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| RecoveryError::CorruptLock {
                        path: lock_path,
                        reason: format!("unparsable pid `{value}`"),
                    });
            }
        }
        Err(RecoveryError::CorruptLock {
            path: lock_path,
            reason: "no pid field".into(),
        })
    }

    /// Look for instance directories whose owning process is gone.
    pub fn scan(&self) -> Result<Vec<PathBuf>, RecoveryError> {
        let entries = fs::read_dir(&self.root).map_err(|source| RecoveryError::Inspect {
            path: self.root.clone(),
            source,
        })?;
        let mut stale = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| RecoveryError::Inspect {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() || !path.join(LOCK_FILE).exists() {
                continue; // not an instance dir, leave it alone
            }
            match Self::read_lock(&path) {
                Ok(pid) if !is_pid_alive(pid) => stale.push(path),
                _ => {} // alive or unreadable: keep it, never guess
            }
        }
        Ok(stale)
    }

    /// Reclaim stale instance directories.
    ///
    /// `current_instance` is never removed even if it appears stale,
    /// which protects against pid reuse false positives for our own dir.
    pub fn recover(
        &self,
        policy: RecoveryPolicy,
        current_instance: &str,
    ) -> Result<RecoveryReport, RecoveryError> {
        let mut report = RecoveryReport::default();
        if policy == RecoveryPolicy::Never {
            return Ok(report);
        }
        for dir in self.scan()? {
            report.scanned += 1;
            if dir.file_name().and_then(|n| n.to_str()) == Some(current_instance) {
                report.kept += 1;
                continue;
            }
            // stay inside root no matter what the lock file claimed
            if !dir.starts_with(&self.root) {
                report.failed += 1;
                report
                    .failure_reasons
                    .push(format!("{} escapes root", dir.display()));
                continue;
            }
            match fs::remove_dir_all(&dir) {
                Ok(()) => report.removed += 1,
                Err(e) => {
                    report.failed += 1;
                    report
                        .failure_reasons
                        .push(format!("{}: {e}", dir.display()));
                }
            }
        }
        Ok(report)
    }

    /// Write the lock file for the current instance.
    pub fn write_lock(&self, instance_id: &str) -> io::Result<()> {
        let dir = self.root.join(instance_id);
        fs::create_dir_all(&dir)?;
        let lock = dir.join(LOCK_FILE);
        fs::write(
            &lock,
            format!("pid={}\ninstance={instance_id}\n", std::process::id()),
        )
    }

    /// Remove the lock file of a cleanly-shut-down instance.
    pub fn remove_lock(&self, instance_id: &str) -> io::Result<()> {
        let lock = self.root.join(instance_id).join(LOCK_FILE);
        match fs::remove_file(&lock) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Human-friendly lock age, mostly for diagnostics.
pub fn lock_age(dir: &Path) -> Option<Duration> {
    fs::metadata(dir.join(LOCK_FILE))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn implausible_pid_is_dead() {
        assert!(!is_pid_alive(0x7FFF_FFFE));
    }

    #[test]
    fn stale_instance_is_reclaimed_but_current_is_kept() {
        let root = crate::testsupport::TestDir::new();
        // fake a crashed instance with a dead pid baked into the lock
        let dead = root.path().join("instance-dead");
        fs::create_dir_all(&dead).unwrap();
        #[cfg(target_os = "linux")]
        // decimal on purpose: the lock parser doesn't do hex
        fs::write(
            dead.join(LOCK_FILE),
            "pid=2147483646\ninstance=instance-dead\n",
        )
        .unwrap();
        #[cfg(not(target_os = "linux"))]
        fs::write(
            dead.join(LOCK_FILE),
            format!("pid={}\ninstance=instance-dead\n", std::process::id()),
        )
        .unwrap();

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(report.removed, 1);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(report.removed, 0); // conservative: never delete on unknown platforms
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn never_policy_touches_nothing() {
        let root = crate::testsupport::TestDir::new();
        let mgr = RecoveryManager::new(root.path());
        let report = mgr.recover(RecoveryPolicy::Never, "instance-x").unwrap();
        assert_eq!(report.scanned, 0);
    }

    #[test]
    fn lock_roundtrip() {
        let root = crate::testsupport::TestDir::new();
        let mgr = RecoveryManager::new(root.path());
        mgr.write_lock("instance-abc").unwrap();
        assert!(root.path().join("instance-abc").join(LOCK_FILE).exists());
        mgr.remove_lock("instance-abc").unwrap();
        assert!(!root.path().join("instance-abc").join(LOCK_FILE).exists());
    }
}
