//! Directory ownership checks.
//!
//! The manager writes into `root` and `root/<instance_id>`. On Unix we
//! refuse to adopt an existing directory owned by a different uid — a
//! root directory (or an instance directory inside it) planted by
//! someone else is exactly what this check is for. Directories we
//! create are owned by us by construction and sail through.
//!
//! Platform split, by design:
//!
//! - **Unix** — `symlink_metadata`/`metadata` via
//!   [`std::os::unix::fs::MetadataExt`] gives the owner uid of the
//!   directory (with symlinks resolved, so a link pointing at a foreign
//!   directory is judged by its target), and the current uid is probed
//!   through `/proc/self/status` on Linux or `id -u` on other Unixes.
//!   Nothing here needs `unsafe` or libc.
//! - **Everything else (Windows included)** — the Rust standard library
//!   exposes no ownership primitive, so only existence can be checked.
//!   That is documented on the builder: existence is the whole check.
//!
//! This is a refuse-before-create gate, not a race-proof guarantee: a
//! hostile actor with write access to the parent directory who swaps
//! things between our check and our first syscall inside it is outside
//! what `std` can defend against. The check stops the common case —
//! adopting a foreign directory wholesale — it does not sandbox an
//! adversarial filesystem. A missing directory is fine: it will be
//! created by us, hence owned by us.

use std::path::Path;

#[cfg(unix)]
use crate::core::error::ConfigError;
use crate::core::error::Result;

/// Verify that `dir`, if it exists, is owned by the current user.
///
/// Returns `Ok(())` for missing directories (they will be created by
/// the caller) and for directories we own. Returns
/// [`ConfigError::DirNotOwned`] when the directory exists and its
/// owner differs from the current uid. On non-Unix platforms this is
/// a no-op: existence is the only check available there.
pub(crate) fn ensure_owned_by_current_user(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        match dir_owner_uid(dir) {
            Ok(Some(owner_uid)) => check_uid(dir, owner_uid, current_uid()),
            Ok(None) => Ok(()),
            Err(source) => Err(ConfigError::RootCreate {
                path: dir.to_path_buf(),
                source,
            }
            .into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Owner uid of `dir`, resolving symlinks. `Ok(None)` when missing.
#[cfg(unix)]
fn dir_owner_uid(dir: &Path) -> std::io::Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(dir) {
        Ok(meta) => Ok(Some(meta.uid())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The decision itself, split out so it can be unit-tested without
/// needing a directory owned by somebody else.
#[cfg(unix)]
fn check_uid(dir: &Path, owner_uid: u32, current_uid: Option<u32>) -> Result<()> {
    let current_uid = current_uid.ok_or_else(|| {
        ConfigError::Invalid(format!(
            "could not determine the current uid to verify the owner of `{}`",
            dir.display()
        ))
    })?;
    if owner_uid == current_uid {
        Ok(())
    } else {
        Err(ConfigError::DirNotOwned {
            dir: dir.to_path_buf(),
            owner_uid,
            current_uid,
        }
        .into())
    }
}

/// Probe the current (effective) uid without libc or `unsafe`.
#[cfg(unix)]
fn current_uid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        // /proc/self/status carries "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("Uid:"))?;
        line.split_whitespace().nth(2)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("id").arg("-u").output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_is_fine() {
        let dir = std::env::temp_dir().join(format!(
            "cachegirl-owner-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(!dir.exists());
        assert!(ensure_owned_by_current_user(&dir).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn matching_uid_is_accepted() {
        let dir = Path::new("/some/dir");
        assert!(check_uid(dir, 1000, Some(1000)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn foreign_uid_is_rejected_with_a_typed_error() {
        let dir = Path::new("/some/dir");
        match check_uid(dir, 0, Some(1000)) {
            Err(crate::core::error::Error::Config(ConfigError::DirNotOwned {
                dir,
                owner_uid,
                current_uid,
            })) => {
                assert_eq!(dir, Path::new("/some/dir"));
                assert_eq!(owner_uid, 0);
                assert_eq!(current_uid, 1000);
            }
            other => panic!("expected DirNotOwned, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unprobeable_current_uid_fails_closed() {
        let dir = Path::new("/some/dir");
        match check_uid(dir, 1000, None) {
            Err(crate::core::error::Error::Config(ConfigError::Invalid(_))) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn directory_we_own_passes_on_this_platform() {
        // whatever the platform can check, a dir we just made must pass
        let dir = std::env::temp_dir().join(format!("cachegirl-owner-ours-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = ensure_owned_by_current_user(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn current_uid_probe_returns_a_plausible_id() {
        // not root-only: just checks the probe works and yields a number
        let uid = current_uid().expect("current uid must be probeable on unix");
        assert!(uid < 1_000_000_000);
    }
}
