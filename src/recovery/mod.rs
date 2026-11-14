//! Crash recovery for instance directories.
//!
//! Each manager instance owns a directory under the shared root and a lock
//! file (`.cachegirl.lock`) recording its pid, the time the instance was
//! created, the instance id, and an opaque process-lifetime token. After an
//! unclean shutdown, stale instance directories can be reclaimed on the next
//! startup — but only when the owning process is *certainly* gone:
//!
//! - the lock's pid does not exist, **or**
//! - the lock's pid exists but its lifetime token does not match the one
//!   recorded (the pid was recycled, so it is not our process anymore), **or**
//! - the directory has no lock file and has not been touched for a
//!   configurable age.
//!
//! If any of these cannot be verified, the directory is reported and left
//! alone. The current instance's directory is never removed. The root must
//! be dedicated to this library; top-level files are ignored.
//!
//! ## Lock file format
//!
//! One `key=value` pair per line; lines starting with `#` are comments and
//! are ignored, as are unknown keys. A value may not contain a newline.
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
//! | Key        | Meaning                                             | Required |
//! |------------|-----------------------------------------------------|----------|
//! | `version`  | Format version (`2`). Ignored by current readers.   | no       |
//! | `pid`      | Owning process id.                                  | yes      |
//! | `created`  | Creation time, seconds since the Unix epoch.        | no       |
//! | `instance` | Instance id; must match the directory name.         | no       |
//! | `start`    | Opaque process-lifetime token (see above).          | no       |
//!
//! A lock that cannot be parsed is treated as corrupt: nothing is removed,
//! and the reason is recorded in the report. Old locks without `created` or
//! `start` still work — liveness then falls back to the pid check alone.

mod manager;
mod process;

pub use manager::{
    DEFAULT_STALE_INSTANCE_AFTER, RecoveryAction, RecoveryEntry, RecoveryManager, RecoveryPolicy,
    RecoveryReport, is_pid_alive, lock_age,
};
