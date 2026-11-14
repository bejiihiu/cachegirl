//! Filesystem backend: real files and directories under a confined root.
//!
//! Safety model:
//! - resource names are validated and can never traverse directories;
//! - all paths are normalized and must stay inside the root;
//! - symlinks are rejected or kept according to the configured policy;
//! - creation never overwrites an existing file.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{ResourceBackend, ResourceRequest};
use crate::core::error::{AccessError, BackendError, Error, ResourceCreationError, Result};
use crate::core::manager::SymlinkPolicy;
use crate::core::resource::{ResourceKind, ResourceMeta};

/// Lexically normalize a path without touching the filesystem.
///
/// Resolves `.` and `..` components by popping path segments. Fails if
/// `..` would step above the path's own root prefix.
pub(crate) fn normalize_lexically(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(BackendError::PathEscape(path.to_path_buf()).into());
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    Ok(out)
}

/// Validate a single path segment (file or directory name).
pub(crate) fn validate_segment(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
    {
        return Err(AccessError::UnsafeName(name.to_owned()).into());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(AccessError::UnsafeName(name.to_owned()).into());
    }
    Ok(())
}

/// Materializes resources as files and directories on disk.
pub struct FilesystemBackend {
    root: PathBuf,
    instance_id: String,
    symlink_policy: SymlinkPolicy,
}

impl FilesystemBackend {
    /// Backend rooted at `root`; all resources live under it.
    ///
    /// `root` is canonicalized so containment checks are reliable.
    pub fn new(
        root: impl AsRef<Path>,
        instance_id: impl Into<String>,
        symlink_policy: SymlinkPolicy,
    ) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref()).map_err(|source| BackendError::Io {
            backend: "filesystem".into(),
            source,
        })?;
        Ok(Self {
            root,
            instance_id: instance_id.into(),
            symlink_policy,
        })
    }

    /// The canonical root this backend confines itself to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve `relative` against the instance namespace dir, keeping it inside the root.
    fn resolve(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        validate_segment(namespace)?;
        validate_segment(name)?;
        let candidate = self.root.join(&self.instance_id).join(namespace).join(name);
        let normalized = normalize_lexically(&candidate)?;
        if !normalized.starts_with(&self.root) {
            return Err(AccessError::OutsideRoot {
                path: normalized,
                root: self.root.clone(),
            }
            .into());
        }
        Ok(normalized)
    }

    fn check_symlink(&self, path: &Path) -> Result<()> {
        let is_symlink = fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink && self.symlink_policy == SymlinkPolicy::Reject {
            return Err(AccessError::SymlinkRejected(path.to_path_buf()).into());
        }
        Ok(())
    }
}

impl ResourceBackend for FilesystemBackend {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn create(&self, request: &ResourceRequest) -> Result<ResourceMeta> {
        if !matches!(request.kind, ResourceKind::File | ResourceKind::Directory) {
            return Err(ResourceCreationError::UnsupportedKind {
                backend: self.name().into(),
                kind: request.kind.to_string(),
            }
            .into());
        }
        let path = self.resolve(&request.namespace, &request.name)?;
        self.check_symlink(&path)?;
        let parent = path
            .parent()
            .expect("resolved path always has a parent")
            .to_path_buf();
        fs::create_dir_all(&parent).map_err(|source| BackendError::Io {
            backend: self.name().into(),
            source,
        })?;

        match request.kind {
            ResourceKind::File => {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true) // never clobber existing data
                    .open(&path)
                    .map_err(|source| match source.kind() {
                        io::ErrorKind::AlreadyExists => Error::Backend(BackendError::Rejected {
                            backend: self.name().into(),
                            reason: format!("`{}` already exists", path.display()),
                        }),
                        _ => Error::Backend(BackendError::Io {
                            backend: self.name().into(),
                            source,
                        }),
                    })?;
            }
            ResourceKind::Directory => {
                fs::create_dir(&path).map_err(|source| match source.kind() {
                    io::ErrorKind::AlreadyExists => Error::Backend(BackendError::Rejected {
                        backend: self.name().into(),
                        reason: format!("`{}` already exists", path.display()),
                    }),
                    _ => Error::Backend(BackendError::Io {
                        backend: self.name().into(),
                        source,
                    }),
                })?;
            }
            _ => unreachable!("kind checked above"),
        }

        let mut meta = ResourceMeta::new(&request.name, &request.namespace, request.kind);
        meta.size_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        meta.metadata
            .insert("path".into(), path.to_string_lossy().into_owned());
        for (k, v) in &request.metadata {
            meta.metadata.insert(k.clone(), v.clone());
        }
        Ok(meta)
    }

    fn release(&self, meta: &ResourceMeta) -> Result<()> {
        let path = match meta.metadata.get("path") {
            Some(p) => PathBuf::from(p),
            None => self.resolve(&meta.namespace, &meta.name)?,
        };
        // never act outside the confined root, wherever the meta came from
        let normalized = normalize_lexically(&path)?;
        if !normalized.starts_with(&self.root) {
            return Err(AccessError::OutsideRoot {
                path: normalized,
                root: self.root.clone(),
            }
            .into());
        }
        let Ok(meta_fs) = fs::symlink_metadata(&normalized) else {
            return Ok(()); // already gone: release is idempotent
        };
        if meta_fs.file_type().is_dir() && !meta_fs.file_type().is_symlink() {
            fs::remove_dir_all(&normalized)
        } else {
            fs::remove_file(&normalized)
        }
        .map_err(|source| {
            BackendError::Io {
                backend: self.name().into(),
                source,
            }
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(root: &Path) -> FilesystemBackend {
        FilesystemBackend::new(root, "instance-test", SymlinkPolicy::Reject).unwrap()
    }

    #[test]
    fn normalize_resolves_dotdot() {
        let p = normalize_lexically(Path::new("/a/b/../c")).unwrap();
        assert_eq!(p, PathBuf::from("/a/c"));
        assert!(normalize_lexically(Path::new("/../escape")).is_err());
    }

    #[test]
    fn unsafe_names_are_rejected() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("../escape", "ns", ResourceKind::File);
        assert!(be.create(&req).is_err());
        let req = ResourceRequest::new("a/b", "ns", ResourceKind::File);
        assert!(be.create(&req).is_err());
    }

    #[test]
    fn create_release_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("data.bin", "ns", ResourceKind::File);
        let meta = be.create(&req).unwrap();
        let path = PathBuf::from(meta.metadata.get("path").unwrap());
        assert!(path.starts_with(be.root()));
        assert!(path.is_file());
        be.release(&meta).unwrap();
        assert!(!path.exists());
        // idempotent release
        be.release(&meta).unwrap();
    }

    #[test]
    fn duplicate_creation_is_rejected() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("data.bin", "ns", ResourceKind::File);
        be.create(&req).unwrap();
        assert!(be.create(&req).is_err());
    }
}
