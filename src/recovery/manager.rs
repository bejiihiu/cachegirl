//! Instance discovery and stale-directory reclamation.
//!
//! Lock format (one `key=value` pair per line, `#` starts a comment):
//!
//! ```text
//! # cachegirl instance lock — do not edit
//! version=2
//! pid=1234
//! created=1763100000
//! instance=instance-abc123
//! start=windows:2026-11-14T10:14:09.9156628Z
//! ```
//!
//! Every field except `pid` is optional; unknown keys are ignored, a torn
//! or unparsable file is reported as corrupt and never leads to deletion.
//! `start` is an opaque process-lifetime token produced by the platform
//! probe; when it does not match the probed token of a live pid, the pid
//! was recycled and the recorded owner is considered gone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::process::{self, ProcessState};
use crate::core::error::RecoveryError;

const LOCK_FILE: &str = ".cachegirl.lock";
const LOCK_VERSION: u32 = 2;
/// Sanity ceiling for lock files; anything larger is treated as corrupt.
const LOCK_MAX_BYTES: u64 = 4096;
/// Default age after which a directory without a lock is considered stale.
pub const DEFAULT_STALE_INSTANCE_AFTER: Duration = Duration::from_secs(3600);

/// How aggressively stale instance directories are reclaimed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RecoveryPolicy {
    /// Never touch other instance directories.
    Never,
    /// Diagnose only: scan, classify and report, but delete nothing.
    ReportOnly,
    /// Reclaim stale directories on startup.
    #[default]
    OnStartup,
}

/// What the recovery pass did with one instance directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RecoveryAction {
    /// The directory was removed because its owner is certainly gone.
    Removed,
    /// The directory was left alone because its owner appears alive.
    Kept,
    /// The directory was left alone; recovery wants a human (or a different
    /// moment) to make the call. See the entry's reason.
    Skipped,
    /// Reclamation was attempted and failed. Other directories were still processed.
    Failed,
}

/// One decision the recovery pass made, with the reason behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecoveryEntry {
    /// Instance directory name (not a full path).
    pub instance: String,
    /// What happened to it.
    pub action: RecoveryAction,
    /// Why this decision was made.
    pub reason: String,
    /// Bytes freed by removing the directory; only set on [`RecoveryAction::Removed`].
    pub bytes_freed: u64,
}

/// Outcome of a recovery pass.
///
/// `skipped`, `bytes_freed`, `entries` and `failure_reasons` default
/// when absent so reports written by older versions still load.
/// Unknown fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecoveryReport {
    /// Instance directories examined.
    pub scanned: u64,
    /// Stale directories removed.
    pub removed: u64,
    /// Directories kept, e.g. because their owner is alive.
    pub kept: u64,
    /// Directories reported but not removed: unverifiable ownership, or
    /// [`RecoveryPolicy::ReportOnly`]. See [`RecoveryReport::entries`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub skipped: u64,
    /// Directories that failed to be reclaimed.
    pub failed: u64,
    /// Bytes freed by reclaiming stale directories.
    #[cfg_attr(feature = "serde", serde(default))]
    pub bytes_freed: u64,
    /// Every decision, in scan order.
    #[cfg_attr(feature = "serde", serde(default))]
    pub entries: Vec<RecoveryEntry>,
    /// Human-readable reasons for the failures.
    #[cfg_attr(feature = "serde", serde(default))]
    pub failure_reasons: Vec<String>,
}

impl std::fmt::Display for RecoveryReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "recovery: scanned {}, removed {} ({} bytes freed), kept {}, skipped {}, failed {}",
            self.scanned, self.removed, self.bytes_freed, self.kept, self.skipped, self.failed
        )?;
        for reason in &self.failure_reasons {
            write!(f, "\n  failure: {reason}")?;
        }
        Ok(())
    }
}

/// Best-effort check whether a process is alive.
///
/// Returns `true` when the answer is "yes" or "we cannot tell" — deleting
/// a live instance's data is far worse than keeping a stale directory.
/// Use the internal probe when you need the three-way answer.
pub fn is_pid_alive(pid: u32) -> bool {
    match process::probe_process(pid) {
        ProcessState::Dead => false,
        ProcessState::Alive { .. } | ProcessState::Unknown { .. } => true,
    }
}

/// Outcome of the stale check for one directory.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// Owner appears alive (or is too young to judge); keep the directory.
    Alive(String),
    /// Owner is certainly gone; the directory may be reclaimed.
    Stale(String),
    /// Cannot make a safe call; keep the directory and report it.
    Unknown(String),
}

/// Parsed lock file. Unknown keys are ignored on purpose; `created` is
/// informational (for humans), so it is not kept.
#[derive(Clone, Debug)]
struct LockContents {
    pid: u32,
    instance: Option<String>,
    start: Option<String>,
}

fn read_lock(dir: &Path) -> Result<LockContents, RecoveryError> {
    let lock_path = dir.join(LOCK_FILE);
    let meta = fs::metadata(&lock_path).map_err(|source| match source.kind() {
        io::ErrorKind::NotFound => RecoveryError::CorruptLock {
            path: lock_path.clone(),
            reason: "lock file is missing".into(),
        },
        _ => RecoveryError::Inspect {
            path: lock_path.clone(),
            source,
        },
    })?;
    if meta.len() > LOCK_MAX_BYTES {
        return Err(RecoveryError::CorruptLock {
            path: lock_path,
            reason: format!(
                "lock file is {} bytes, expected at most {LOCK_MAX_BYTES}",
                meta.len()
            ),
        });
    }
    let raw = fs::read(&lock_path).map_err(|source| RecoveryError::Inspect {
        path: lock_path.clone(),
        source,
    })?;
    let raw = String::from_utf8(raw).map_err(|_| RecoveryError::CorruptLock {
        path: lock_path.clone(),
        reason: "lock file is not valid UTF-8".into(),
    })?;
    parse_lock(&raw, &lock_path)
}

fn parse_lock(raw: &str, lock_path: &Path) -> Result<LockContents, RecoveryError> {
    let corrupt = |reason: String| RecoveryError::CorruptLock {
        path: lock_path.to_path_buf(),
        reason,
    };
    let mut pid = None;
    let mut instance = None;
    let mut start = None;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue; // tolerate stray lines rather than brick an old lock
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "pid" => {
                pid = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| corrupt(format!("unparsable pid `{value}`")))?,
                )
            }
            "created" => {} // informational, not needed for the decision
            "instance" => instance = (!value.is_empty()).then(|| value.to_owned()),
            "start" => start = (!value.is_empty()).then(|| value.to_owned()),
            _ => {} // forward-compatible: newer keys are ignored
        }
    }
    let pid = pid.ok_or_else(|| corrupt("no pid field".into()))?;
    Ok(LockContents {
        pid,
        instance,
        start,
    })
}

/// Decide what to do with a directory that has a lock file.
fn decide_locked(contents: &LockContents, dir_name: &str) -> Verdict {
    if let Some(instance) = &contents.instance {
        if instance != dir_name {
            // could be a renamed or misplaced directory — not ours to judge
            return Verdict::Unknown(format!(
                "lock names instance `{instance}` but the directory is `{dir_name}`; not touching it"
            ));
        }
    }
    let pid = contents.pid;
    match process::probe_process(pid) {
        ProcessState::Dead => Verdict::Stale(format!("pid {pid} is not running")),
        ProcessState::Alive {
            start_token: Some(actual),
        } => match &contents.start {
            Some(recorded) if *recorded != actual => Verdict::Stale(format!(
                "pid {pid} is alive but its lifetime token does not match the lock (pid was reused)"
            )),
            _ => Verdict::Alive(format!("pid {pid} is running")),
        },
        ProcessState::Alive { start_token: None } => Verdict::Alive(format!(
            "pid {pid} is running (lifetime token unavailable; cannot rule out reuse)"
        )),
        ProcessState::Unknown { reason } => {
            Verdict::Unknown(format!("could not verify pid {pid}: {reason}"))
        }
    }
}

/// Decide what to do with a directory that has no lock file.
fn decide_unlocked(
    dir_modified: Option<SystemTime>,
    threshold: Duration,
    now: SystemTime,
) -> Verdict {
    let Some(modified) = dir_modified else {
        return Verdict::Unknown("no lock file and the directory mtime is unavailable".into());
    };
    match now.duration_since(modified) {
        Ok(age) if age >= threshold => Verdict::Stale(format!(
            "no lock file and directory is {age:?} old (threshold {threshold:?})"
        )),
        Ok(age) => Verdict::Unknown(format!(
            "no lock file but directory is only {age:?} old (threshold {threshold:?}); left alone"
        )),
        Err(_) => Verdict::Unknown(
            "no lock file and the directory mtime is in the future; left alone".into(),
        ),
    }
}

/// Scan a root directory for stale instance directories and reclaim them.
///
/// The root is expected to be dedicated to this library: every directory
/// directly inside it is a candidate for an instance directory. Files at
/// the top level are ignored. Directories are never followed (symlinks to
/// directories are skipped), and every candidate is re-validated to sit
/// directly inside the canonical root before anything is removed.
pub struct RecoveryManager {
    root: PathBuf,
    stale_after: Duration,
}

impl RecoveryManager {
    /// Recovery rooted at a shared base directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            stale_after: DEFAULT_STALE_INSTANCE_AFTER,
        }
    }

    /// Override how old a lock-less directory must be before it is stale.
    pub fn with_stale_after(mut self, threshold: Duration) -> Self {
        self.stale_after = threshold;
        self
    }

    /// The configured age after which a lock-less stale directory is reclaimed.
    pub fn stale_after(&self) -> Duration {
        self.stale_after
    }

    fn entry_name(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }

    fn tally(
        report: &mut RecoveryReport,
        dir: &Path,
        action: RecoveryAction,
        reason: String,
        bytes_freed: u64,
    ) {
        let name = Self::entry_name(dir);
        #[cfg(feature = "tracing")]
        match action {
            RecoveryAction::Removed => {
                tracing::info!(instance = %name, bytes_freed, reason = %reason, "reclaimed stale instance directory");
            }
            RecoveryAction::Failed => {
                tracing::warn!(instance = %name, reason = %reason, "could not reclaim stale instance directory");
            }
            RecoveryAction::Kept | RecoveryAction::Skipped => {
                tracing::debug!(instance = %name, action = ?action, reason = %reason, "instance directory kept");
            }
        }
        match action {
            RecoveryAction::Removed => {
                report.removed += 1;
                report.bytes_freed += bytes_freed;
            }
            RecoveryAction::Kept => report.kept += 1,
            RecoveryAction::Skipped => report.skipped += 1,
            RecoveryAction::Failed => {
                report.failed += 1;
                report.failure_reasons.push(format!("{name}: {reason}"));
            }
        }
        report.entries.push(RecoveryEntry {
            instance: name,
            action,
            reason,
            bytes_freed,
        });
    }

    /// Classify one instance candidate. Never fails; problems surface as [`Verdict::Unknown`].
    fn examine(dir: &Path, stale_after: Duration) -> Verdict {
        let name = Self::entry_name(dir);
        let lock_path = dir.join(LOCK_FILE);
        if lock_path.is_file() {
            return match read_lock(dir) {
                Ok(contents) => decide_locked(&contents, &name),
                Err(e) => Verdict::Unknown(e.to_string()),
            };
        }
        if lock_path.exists() {
            return Verdict::Unknown("lock file exists but is not a regular file".into());
        }
        let modified = fs::symlink_metadata(dir).and_then(|m| m.modified()).ok();
        decide_unlocked(modified, stale_after, SystemTime::now())
    }

    fn dir_size(path: &Path) -> u64 {
        let mut total = 0u64;
        if let Ok(rd) = fs::read_dir(path) {
            for entry in rd.flatten() {
                match fs::symlink_metadata(entry.path()) {
                    Ok(meta) if meta.file_type().is_dir() => total += Self::dir_size(&entry.path()),
                    Ok(meta) => total += meta.len(),
                    Err(_) => {}
                }
            }
        }
        total
    }

    /// Instance directories whose owner is (or was) certainly gone,
    /// according to the configured policy. Informational only —
    /// [`RecoveryManager::recover`] performs the bookkeeping and the deletion.
    pub fn scan(&self) -> Result<Vec<PathBuf>, RecoveryError> {
        let root = self.canonical_root()?;
        let mut report = RecoveryReport::default();
        let dirs = self.candidates(&root, &mut report);
        Ok(dirs
            .into_iter()
            .filter(|dir| matches!(Self::examine(dir, self.stale_after), Verdict::Stale(_)))
            .collect())
    }

    fn canonical_root(&self) -> Result<PathBuf, RecoveryError> {
        fs::canonicalize(&self.root).map_err(|source| RecoveryError::Inspect {
            path: self.root.clone(),
            source,
        })
    }

    /// Direct children of the root that are real directories.
    /// Files and symlinks at the top level are not instance candidates.
    /// Entries that cannot be stat'ed are tallied as failed and skipped.
    fn candidates(&self, root: &Path, report: &mut RecoveryReport) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(source) => {
                // caller maps this to a hard error when the root itself is unreadable
                let reason = format!("could not read the recovery root: {source}");
                report.failed += 1;
                report.failure_reasons.push(reason.clone());
                report.entries.push(RecoveryEntry {
                    instance: root.display().to_string(),
                    action: RecoveryAction::Failed,
                    reason,
                    bytes_freed: 0,
                });
                return dirs;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    if is_dir {
                        dirs.push(entry.path());
                    }
                }
                Err(source) => {
                    let reason = format!("could not read a directory entry: {source}");
                    report.failed += 1;
                    report.failure_reasons.push(reason.clone());
                    report.entries.push(RecoveryEntry {
                        instance: "<unreadable entry>".into(),
                        action: RecoveryAction::Failed,
                        reason,
                        bytes_freed: 0,
                    });
                }
            }
        }
        dirs
    }

    /// Shared pass. Nothing is removed when the policy is
    /// [`RecoveryPolicy::ReportOnly`]; stale entries are reported instead.
    fn run(
        &self,
        root: &Path,
        policy: RecoveryPolicy,
        current_instance: &str,
        report: &mut RecoveryReport,
    ) {
        for dir in self.candidates(root, report) {
            let name = Self::entry_name(&dir);
            report.scanned += 1;
            if name == current_instance {
                Self::tally(
                    report,
                    &dir,
                    RecoveryAction::Kept,
                    "current instance".into(),
                    0,
                );
                continue;
            }
            // the directory must be a real directory sitting directly in the root
            let canonical = match fs::canonicalize(&dir) {
                Ok(c) => c,
                Err(e) => {
                    Self::tally(
                        report,
                        &dir,
                        RecoveryAction::Failed,
                        format!("could not canonicalize: {e}"),
                        0,
                    );
                    continue;
                }
            };
            if canonical.parent() != Some(root) {
                Self::tally(
                    report,
                    &dir,
                    RecoveryAction::Failed,
                    "escapes the recovery root".into(),
                    0,
                );
                continue;
            }

            match Self::examine(&dir, self.stale_after) {
                Verdict::Alive(reason) => {
                    Self::tally(report, &dir, RecoveryAction::Kept, reason, 0)
                }
                Verdict::Unknown(reason) => {
                    Self::tally(report, &dir, RecoveryAction::Skipped, reason, 0)
                }
                Verdict::Stale(reason) => {
                    if policy == RecoveryPolicy::ReportOnly {
                        Self::tally(
                            report,
                            &dir,
                            RecoveryAction::Skipped,
                            format!("{reason}; report-only mode, not removing"),
                            0,
                        );
                        continue;
                    }
                    let bytes = Self::dir_size(&dir);
                    match fs::remove_dir_all(&dir) {
                        Ok(()) => Self::tally(report, &dir, RecoveryAction::Removed, reason, bytes),
                        Err(e) => Self::tally(
                            report,
                            &dir,
                            RecoveryAction::Failed,
                            format!("{reason}; removal failed: {e}"),
                            0,
                        ),
                    }
                }
            }
        }
    }

    /// Reclaim stale instance directories.
    ///
    /// `current_instance` is never removed even if it appears stale,
    /// which protects against pid reuse false positives for our own dir.
    /// Per-directory failures are collected in the report and never stop
    /// the pass; a top-level failure (unreadable root) is returned as an error.
    #[cfg_attr(feature = "tracing", tracing::instrument(
        skip(self, policy, current_instance),
        name = "recovery",
        level = "info",
        fields(instance = current_instance),
    ))]
    pub fn recover(
        &self,
        policy: RecoveryPolicy,
        current_instance: &str,
    ) -> Result<RecoveryReport, RecoveryError> {
        let mut report = RecoveryReport::default();
        if policy == RecoveryPolicy::Never {
            return Ok(report);
        }
        let root = self.canonical_root()?;
        self.run(&root, policy, current_instance, &mut report);
        Ok(report)
    }

    /// Write the lock file for the current instance.
    pub fn write_lock(&self, instance_id: &str) -> io::Result<()> {
        let dir = self.root.join(instance_id);
        fs::create_dir_all(&dir)?;
        let lock = dir.join(LOCK_FILE);
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut body = String::new();
        body.push_str("# cachegirl instance lock — do not edit\n");
        body.push_str(&format!("version={LOCK_VERSION}\n"));
        body.push_str(&format!("pid={}\n", std::process::id()));
        body.push_str(&format!("created={created}\n"));
        body.push_str(&format!("instance={instance_id}\n"));
        if let Some(token) = process::current_start_token() {
            body.push_str(&format!("start={token}\n"));
        }
        // written directly on purpose: no rename gap, and a torn read is
        // indistinguishable from a corrupt lock, which readers must tolerate anyway
        fs::write(&lock, body)
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

    fn test_root() -> crate::testsupport::TestDir {
        crate::testsupport::TestDir::new()
    }

    /// An out-of-range pid: beyond any realistic pid on Linux or Windows.
    const ABSURD_PID: u32 = 0x7FFF_FFFE;

    fn write_lock_file(dir: &Path, pid: u32, instance: &str, start: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        let start_line = start.map(|s| format!("start={s}\n")).unwrap_or_default();
        fs::write(
            dir.join(LOCK_FILE),
            format!("# cachegirl instance lock — do not edit\nversion={LOCK_VERSION}\npid={pid}\ncreated=1763100000\ninstance={instance}\n{start_line}"),
        )
        .unwrap();
    }

    fn write_file_sized(dir: &Path, name: &str, bytes: usize) {
        fs::write(dir.join(name), vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn current_process_is_alive() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn absurd_pid_is_dead_on_supported_platforms() {
        // on Linux and Windows tasklist we can say for sure; elsewhere the
        // conservative answer is also acceptable to callers
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(!is_pid_alive(ABSURD_PID));
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = ABSURD_PID;
    }

    #[test]
    fn reaped_child_pid_is_dead() {
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!is_pid_alive(pid));
    }

    #[test]
    fn probe_agrees_with_current_process() {
        let token = process::current_start_token()
            .expect("current process must have a lifetime token on this platform");
        match process::probe_process(std::process::id()) {
            ProcessState::Alive { start_token } => {
                assert_eq!(start_token.as_deref(), Some(token.as_str()))
            }
            other => panic!("current process reported {other:?}"),
        }
    }

    #[test]
    fn stale_instance_with_dead_pid_is_reclaimed_and_bytes_reported() {
        let root = test_root();
        let dead = root.path().join("instance-dead");
        write_lock_file(&dead, ABSURD_PID, "instance-dead", None);
        write_file_sized(&dead, "data.bin", 4096);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 1, "report: {report}");
        assert_eq!(report.scanned, 1);
        assert_eq!(report.kept, 0);
        assert!(report.bytes_freed >= 4096);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].action, RecoveryAction::Removed);
        assert!(!dead.exists());
    }

    #[test]
    fn pid_reuse_lock_with_live_pid_and_stale_token_is_reclaimed() {
        let root = test_root();
        let reused = root.path().join("instance-reused");
        // pid is us (alive), but the recorded token is from a process lifetime
        // that ended long ago — the pid must have been recycled
        write_lock_file(
            &reused,
            std::process::id(),
            "instance-reused",
            Some("epoch-of-the-long-gone"),
        );

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-other")
            .unwrap();
        assert_eq!(report.removed, 1, "report: {report}");
        assert!(!reused.exists());
    }

    #[test]
    fn current_live_instance_is_never_removed() {
        let root = test_root();
        let live = root.path().join("instance-live");
        let token = process::current_start_token();
        write_lock_file(&live, std::process::id(), "instance-live", token.as_deref());

        let mgr = RecoveryManager::new(root.path());
        // current instance keeps itself even with a bogus token: current is sacred
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-live")
            .unwrap();
        assert_eq!(report.kept, 1);
        assert!(live.exists());

        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        // pid is alive and the token matches: kept, regardless of the current-instance shield
        assert_eq!(report.kept, 1);
        assert!(live.exists());
    }

    #[test]
    fn corrupt_lock_is_never_reclaimed_and_reason_is_reported() {
        let root = test_root();
        let broken = root.path().join("instance-broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(LOCK_FILE), "this is not a lock file\n").unwrap();
        write_file_sized(&broken, "precious.bin", 128);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.entries[0].action, RecoveryAction::Skipped);
        assert!(
            broken.join("precious.bin").exists(),
            "corrupt lock must not delete data"
        );
    }

    #[test]
    fn lock_with_unparsable_pid_is_never_reclaimed() {
        let root = test_root();
        let broken = root.path().join("instance-badpid");
        write_lock_file(&broken, 1234, "instance-badpid", None);
        fs::write(
            broken.join(LOCK_FILE),
            "pid=not-a-number\ninstance=instance-badpid\n",
        )
        .unwrap();

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(broken.exists());
    }

    #[test]
    fn lock_belonging_to_a_different_instance_name_is_left_alone() {
        let root = test_root();
        let misplaced = root.path().join("instance-a");
        write_lock_file(&misplaced, ABSURD_PID, "instance-b", None);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(misplaced.exists());
    }

    #[test]
    fn legacy_lock_without_token_falls_back_to_liveness_only() {
        let root = test_root();
        let live = root.path().join("instance-legacy");
        write_lock_file(&live, std::process::id(), "instance-legacy", None);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.kept, 1);
        assert!(live.exists());
    }

    #[test]
    fn report_only_mode_reports_but_does_not_delete() {
        let root = test_root();
        let dead = root.path().join("instance-dead");
        write_lock_file(&dead, ABSURD_PID, "instance-dead", None);
        write_file_sized(&dead, "data.bin", 1024);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::ReportOnly, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.bytes_freed, 0);
        assert_eq!(report.entries[0].action, RecoveryAction::Skipped);
        assert!(dead.join("data.bin").exists());
    }

    #[test]
    fn never_policy_touches_nothing() {
        let root = test_root();
        let dead = root.path().join("instance-dead");
        write_lock_file(&dead, ABSURD_PID, "instance-dead", None);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr.recover(RecoveryPolicy::Never, "instance-x").unwrap();
        assert_eq!(report.scanned, 0);
        assert!(dead.exists());
    }

    #[test]
    fn stale_directory_without_lock_and_old_enough_is_reclaimed() {
        let root = test_root();
        let old = root.path().join("instance-ancient");
        fs::create_dir_all(&old).unwrap();
        write_file_sized(&old, "data.bin", 256);

        // with a zero threshold even a brand-new lock-less directory counts as stale
        let mgr = RecoveryManager::new(root.path()).with_stale_after(Duration::from_secs(0));
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 1);
        assert!(!old.exists());
    }

    #[test]
    fn young_directory_without_lock_is_left_alone() {
        let root = test_root();
        let young = root.path().join("instance-young");
        fs::create_dir_all(&young).unwrap();
        write_file_sized(&young, "data.bin", 256);

        let mgr = RecoveryManager::new(root.path())
            .with_stale_after(Duration::from_secs(365 * 24 * 3600));
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(young.exists());
    }

    #[test]
    fn unlocked_decision_respects_thresholds() {
        let now = SystemTime::now();
        let old_enough = now - Duration::from_secs(7200);
        let just_now = now - Duration::from_secs(1);
        assert!(matches!(
            decide_unlocked(Some(old_enough), Duration::from_secs(3600), now),
            Verdict::Stale(_)
        ));
        assert!(matches!(
            decide_unlocked(Some(just_now), Duration::from_secs(3600), now),
            Verdict::Unknown(_)
        ));
        assert!(matches!(
            decide_unlocked(None, Duration::from_secs(3600), now),
            Verdict::Unknown(_)
        ));
        assert!(matches!(
            decide_unlocked(
                Some(now + Duration::from_secs(3600)),
                Duration::from_secs(3600),
                now
            ),
            Verdict::Unknown(_)
        ));
    }

    #[test]
    fn one_failing_instance_does_not_stop_the_others() {
        let root = test_root();
        let dead_a = root.path().join("instance-dead-a");
        let dead_b = root.path().join("instance-dead-b");
        write_lock_file(&dead_a, ABSURD_PID, "instance-dead-a", None);
        write_lock_file(&dead_b, ABSURD_PID, "instance-dead-b", None);

        // make the first candidate undeletable: on unix a subdirectory this
        // process cannot write to; on Windows deletion failure is not
        // reproducible through read-only attributes, so there we document
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = dead_a.join("locked-sub");
            fs::create_dir_all(&sub).unwrap();
            fs::write(sub.join("f.bin"), b"x").unwrap();
            fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
        }

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        #[cfg(unix)]
        {
            assert_eq!(report.failed, 1, "report: {report}");
            assert_eq!(
                report.removed, 1,
                "the healthy instance must still be reclaimed"
            );
            // restore writability so the temp dir can clean itself up
            use std::os::unix::fs::PermissionsExt;
            let _ =
                fs::set_permissions(dead_a.join("locked-sub"), fs::Permissions::from_mode(0o700));
        }
        #[cfg(not(unix))]
        {
            assert_eq!(report.failed, 0);
            assert_eq!(report.removed, 2);
        }
    }

    #[test]
    fn failure_outside_root_never_happens_via_scan() {
        // every candidate comes from read_dir(root), so it is inside the root
        // by construction; this test pins the invariant anyway
        let root = test_root();
        let outside = test_root();
        let victim = outside.path().join("not-inside");
        write_lock_file(&victim, ABSURD_PID, "instance-victim", None);

        let mgr = RecoveryManager::new(root.path());
        let report = mgr
            .recover(RecoveryPolicy::OnStartup, "instance-current")
            .unwrap();
        assert_eq!(report.scanned, 0);
        assert!(
            victim.exists(),
            "directories outside the root must never be touched"
        );
    }

    #[test]
    fn lock_roundtrip_and_format() {
        let root = test_root();
        let mgr = RecoveryManager::new(root.path());
        mgr.write_lock("instance-abc").unwrap();
        let lock = root.path().join("instance-abc").join(LOCK_FILE);
        let raw = fs::read_to_string(&lock).unwrap();
        assert!(raw.contains(&format!("pid={}", std::process::id())));
        assert!(raw.contains("instance=instance-abc"));
        assert!(raw.contains("created="));
        assert!(raw.contains("version=2"));
        mgr.remove_lock("instance-abc").unwrap();
        assert!(!lock.exists());
        // removal is idempotent
        mgr.remove_lock("instance-abc").unwrap();
    }

    #[test]
    fn scanned_currently_wired_to_candidates() {
        let root = test_root();
        fs::create_dir_all(root.path().join("plain-dir-no-lock")).unwrap();
        fs::write(root.path().join("a-file.txt"), b"hi").unwrap();
        let dead = root.path().join("instance-dead");
        write_lock_file(&dead, ABSURD_PID, "instance-dead", None);

        let mgr = RecoveryManager::new(root.path()).with_stale_after(Duration::from_secs(0));
        let stale = mgr.scan().unwrap();
        assert_eq!(stale.len(), 2, "dead-pid lock dir and lock-less dir");
        let report = mgr
            .recover(RecoveryPolicy::ReportOnly, "instance-current")
            .unwrap();
        assert_eq!(
            report.scanned, 2,
            "files at the top level are not candidates"
        );
        assert_eq!(report.skipped, 2);
    }

    #[test]
    fn parse_lock_survives_unknown_keys_and_comments() {
        let parsed = parse_lock(
            "# comment\n\nversion=2\npid=4242\ncreated=1763100000\ninstance=i-1\nstart=tok\nfuture-key=ignored\n",
            Path::new("lock"),
        )
        .unwrap();
        assert_eq!(parsed.pid, 4242);
        assert_eq!(parsed.instance.as_deref(), Some("i-1"));
        assert_eq!(parsed.start.as_deref(), Some("tok"));
    }

    #[test]
    fn parse_lock_rejects_missing_or_garbage_pid() {
        assert!(parse_lock("instance=i-1\n", Path::new("lock")).is_err());
        assert!(parse_lock("pid=\n", Path::new("lock")).is_err());
        assert!(parse_lock("pid=12x4\n", Path::new("lock")).is_err());
        // garbage in optional fields must not matter at all
        let parsed = parse_lock("pid=7\ncreated=banana\nstart=\n", Path::new("lock")).unwrap();
        assert_eq!(parsed.start, None);
    }
}
