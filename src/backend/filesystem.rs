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
use crate::namespace::is_reserved_device_name;

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
///
/// On top of the traversal and control-character rules, `:` is rejected
/// (on NTFS it silently denotes an alternate data stream) and names may
/// not end with `.`: Windows strips trailing dots, which would make
/// `probe` and `probe.` alias the same file.
pub(crate) fn validate_segment(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
    {
        return Err(AccessError::UnsafeName(name.to_owned()).into());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(AccessError::UnsafeName(name.to_owned()).into());
    }
    if name.ends_with('.') {
        return Err(AccessError::UnsafeName(name.to_owned()).into());
    }
    if is_reserved_device_name(name) {
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
    ///
    /// The namespace may be a path (`parent/sub`); every segment is
    /// validated individually, so nested namespaces materialize under
    /// `root/<instance>/parent/sub/` and no segment can traverse out.
    fn resolve(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        validate_segment(name)?;
        let mut candidate = self.root.join(&self.instance_id);
        for segment in crate::namespace::validate_namespace_path(namespace)
            .map_err(|_| AccessError::UnsafeName(namespace.to_owned()))?
        {
            candidate.push(segment);
        }
        candidate.push(name);
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

    /// Every existing component under `parent` must resolve inside the
    /// root.
    ///
    /// Lexical `starts_with` cannot see a symlinked intermediate
    /// directory, so walk up to the deepest existing ancestor and
    /// canonicalize it. A path whose every resolvable ancestor is
    /// confined is allowed; the moment one resolves outside, we bail
    /// out. Called with the parent of the path being created or
    /// removed on purpose, so a symlink as the final component never
    /// reaches `canonicalize` (the caller decides what to do with
    /// links themselves).
    fn ensure_existing_ancestor_inside(&self, parent: &Path) -> Result<()> {
        let mut probe = parent.to_path_buf();
        loop {
            match fs::canonicalize(&probe) {
                Ok(canon) => {
                    if canon.starts_with(&self.root) {
                        return Ok(());
                    }
                    return Err(AccessError::OutsideRoot {
                        path: canon,
                        root: self.root.clone(),
                    }
                    .into());
                }
                Err(_) => {
                    if !probe.pop() {
                        // nothing on the path exists yet; the root itself
                        // is canonical and confined by construction
                        return Ok(());
                    }
                }
            }
        }
    }

    fn check_symlink(&self, path: &Path) -> Result<()> {
        let is_symlink = fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !is_symlink {
            return Ok(());
        }
        match self.symlink_policy {
            SymlinkPolicy::Reject => Err(AccessError::SymlinkRejected(path.to_path_buf()).into()),
            SymlinkPolicy::WithinRoot => {
                // a dangling link can't be verified either: refuse
                let canon = fs::canonicalize(path).map_err(|_| AccessError::OutsideRoot {
                    path: path.to_path_buf(),
                    root: self.root.clone(),
                })?;
                if canon.starts_with(&self.root) {
                    Ok(())
                } else {
                    Err(AccessError::OutsideRoot {
                        path: canon,
                        root: self.root.clone(),
                    }
                    .into())
                }
            }
            SymlinkPolicy::Allow => Ok(()),
        }
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
        // the final component may not exist yet, but every component up
        // to the deepest existing ancestor must stay inside the root —
        // a planted symlinked directory must not swallow the file
        self.ensure_existing_ancestor_inside(&parent)?;
        fs::create_dir_all(&parent).map_err(|source| BackendError::Io {
            backend: self.name().into(),
            source,
        })?;
        // re-check after the mkdir: the directory may have been planted
        // or swapped between the two calls
        self.ensure_existing_ancestor_inside(&parent)?;

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
        // a lexically-confined path can still walk out through a
        // symlinked parent; verify every resolvable ancestor stays inside
        if let Some(parent) = normalized.parent() {
            self.ensure_existing_ancestor_inside(parent)?;
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

    #[test]
    fn validate_segment_accepts_unicode_and_long_names() {
        assert!(validate_segment("файл.bin").is_ok());
        assert!(validate_segment("☃-snowman.txt").is_ok());
        let long = "x".repeat(1000);
        assert!(validate_segment(&long).is_ok());
        // dots inside a name are fine, only trailing ones alias on windows
        assert!(validate_segment("archive.tar.gz").is_ok());
        assert!(validate_segment(".hidden").is_ok());
    }

    #[test]
    fn validate_segment_rejects_trailing_dots_and_colons() {
        // windows strips trailing dots, aliasing "name" and "name."
        assert!(validate_segment("probe.").is_err());
        assert!(validate_segment("...").is_err());
        assert!(validate_segment("archive.tar.").is_err());
        // ':' silently names an alternate data stream on NTFS
        assert!(validate_segment("a:b").is_err());
        assert!(validate_segment(":stream").is_err());
    }

    #[test]
    fn validate_segment_rejects_mixed_separators() {
        use super::validate_segment as v;
        assert!(v("a/b").is_err());
        assert!(v("a\\b").is_err());
        assert!(v("a/b\\c").is_err());
        assert!(v("a\\b/c").is_err());
    }

    #[test]
    fn validate_segment_rejects_control_characters_and_padding() {
        use super::validate_segment as v;
        assert!(v("a\u{0}b").is_err());
        assert!(v("a\u{7f}b").is_err());
        assert!(v("a\u{1f}b").is_err());
        assert!(v(" padded").is_err());
        assert!(v("padded ").is_err());
        assert!(v("a\tb").is_err());
    }

    #[test]
    fn normalize_resolves_dot_and_duplicate_separators() {
        let p = normalize_lexically(Path::new("/a/./b")).unwrap();
        assert_eq!(p, PathBuf::from("/a/b"));
        assert_eq!(
            normalize_lexically(Path::new("/a/.")).unwrap(),
            PathBuf::from("/a")
        );
        assert_eq!(
            normalize_lexically(Path::new("/a/b/../..")).unwrap(),
            PathBuf::from("/")
        );
        // ".." must never climb above the anchor
        assert!(normalize_lexically(Path::new("/a/../..")).is_err());
        assert!(normalize_lexically(Path::new("..")).is_err());
    }

    #[test]
    fn release_outside_the_root_is_refused_and_keeps_the_file() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let outside = crate::testsupport::TestDir::new();
        let victim = outside.path().join("victim.bin");
        std::fs::write(&victim, b"precious").unwrap();

        // a meta record pointing straight at a file outside the backend
        // root must never authorize deleting it
        let mut meta = ResourceMeta::new("victim.bin", "ns", ResourceKind::File);
        meta.metadata
            .insert("path".into(), victim.to_string_lossy().into_owned());
        assert!(be.release(&meta).is_err());
        assert!(victim.exists());

        // same, but the path escapes through ".."
        let escape = dir
            .path()
            .join("..")
            .join(outside.path().file_name().unwrap())
            .join("victim.bin");
        let mut meta = ResourceMeta::new("victim.bin", "ns", ResourceKind::File);
        meta.metadata
            .insert("path".into(), escape.to_string_lossy().into_owned());
        assert!(be.release(&meta).is_err());
        assert!(victim.exists());
    }

    #[test]
    fn release_with_dotdot_that_stays_inside_the_root_works() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("data.bin", "ns", ResourceKind::File);
        let meta = be.create(&req).unwrap();
        let path = PathBuf::from(meta.metadata.get("path").unwrap());
        // reroute the record through a redundant "ns/../ns" detour that
        // normalizes back inside the root
        let detour = be
            .root()
            .join("instance-test")
            .join("ns")
            .join("..")
            .join("ns")
            .join("data.bin");
        let mut detoured = meta.clone();
        detoured
            .metadata
            .insert("path".into(), detour.to_string_lossy().into_owned());
        be.release(&detoured).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn release_removes_a_directory_tree() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("workdir", "ns", ResourceKind::Directory);
        let meta = be.create(&req).unwrap();
        let path = PathBuf::from(meta.metadata.get("path").unwrap());
        std::fs::create_dir_all(path.join("nested/deep")).unwrap();
        std::fs::write(path.join("nested/deep/f.bin"), b"x").unwrap();
        be.release(&meta).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn create_rejects_a_name_with_a_colon() {
        let dir = crate::testsupport::TestDir::new();
        let be = backend(dir.path());
        let req = ResourceRequest::new("a:b", "ns", ResourceKind::File);
        assert!(be.create(&req).is_err());
        let req = ResourceRequest::new("probe.", "ns", ResourceKind::File);
        assert!(be.create(&req).is_err());
    }
}
