//! A file-backed resource handle.
//!
//! What happens on drop depends on the ownership mode the resource was
//! created with: `Managed` and `Leased` handles detach without touching
//! the file, `Owned` and `Shared` handles destroy it. Physical deletion
//! always goes through the backend and always stands down while leases
//! are held.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::core::error::ConcurrencyError;
use crate::core::resource::ResourceId;
use crate::ownership::{Lease, OwnershipCore};

/// Lightweight handle to a registered file resource.
#[derive(Clone, Debug)]
pub struct FileResource {
    id: ResourceId,
    path: PathBuf,
    core: Arc<OwnershipCore>,
}

impl FileResource {
    // internal: handles are minted by the manager, not assembled by hand
    pub(crate) fn new(id: ResourceId, path: impl Into<PathBuf>, core: Arc<OwnershipCore>) -> Self {
        Self {
            id,
            path: path.into(),
            core,
        }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Acquire a lease on the resource.
    ///
    /// While at least one lease is held, the resource cannot be
    /// physically deleted: explicit release and cleanup passes move it
    /// to `PendingCleanup` instead. Dropping the lease releases it.
    ///
    /// Fails with [`ConcurrencyError::Destroying`] if the resource is
    /// being destroyed right now, or [`ConcurrencyError::Poisoned`] if
    /// the internal book was poisoned by a panic.
    ///
    /// # Example
    ///
    /// ```
    /// # use cachegirl::TempManager;
    /// # let dir = std::env::temp_dir().join(format!("cachegirl-acquire-{}", std::process::id()));
    /// # let manager = TempManager::builder().root(&dir).build()?;
    /// # let ns = manager.namespace("protocol")?;
    /// let file = ns.create_file("packet.bin")?;
    /// let lease = file.acquire()?;
    /// process(&file);
    /// drop(lease);
    /// # fn process(_: &cachegirl::FileResource) {}
    /// # manager.shutdown()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok::<(), cachegirl::Error>(())
    /// ```
    pub fn acquire(&self) -> Result<Lease, ConcurrencyError> {
        self.core.acquire_lease()
    }

    /// Path on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current file size, or an I/O error if it is gone.
    pub fn len(&self) -> io::Result<u64> {
        Ok(fs::metadata(&self.path)?.len())
    }

    /// True when the file exists and is empty.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// True when the path still exists (file or directory).
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Last modification time reported by the filesystem.
    pub fn modified(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.modified()
    }
}

/// Lightweight handle to a registered directory resource.
#[derive(Clone, Debug)]
pub struct DirectoryResource {
    id: ResourceId,
    path: PathBuf,
    core: Arc<OwnershipCore>,
}

impl DirectoryResource {
    // internal: handles are minted by the manager, not assembled by hand
    pub(crate) fn new(id: ResourceId, path: impl Into<PathBuf>, core: Arc<OwnershipCore>) -> Self {
        Self {
            id,
            path: path.into(),
            core,
        }
    }

    /// Resource id in the registry.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Path on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Acquire a lease on the resource.
    ///
    /// While at least one lease is held, the resource cannot be
    /// physically deleted: explicit release and cleanup passes move it
    /// to `PendingCleanup` instead. Dropping the lease releases it.
    pub fn acquire(&self) -> Result<Lease, ConcurrencyError> {
        self.core.acquire_lease()
    }

    /// True when the directory still exists.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// Last modification time reported by the filesystem.
    pub fn modified(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.modified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_core(dir: &crate::testsupport::TestDir, id: &ResourceId) -> Arc<OwnershipCore> {
        let shared = crate::namespace::test_shared(dir.path());
        Arc::new(OwnershipCore::new(
            shared,
            id.clone(),
            crate::core::resource::Ownership::Managed,
        ))
    }

    #[test]
    fn metadata_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let path = dir.path().join("x.bin");
        fs::write(&path, b"hello").unwrap();
        let id = ResourceId::new();
        let core = test_core(&dir, &id);
        let res = FileResource::new(id.clone(), &path, core);
        assert!(res.exists());
        assert_eq!(res.len().unwrap(), 5);
        assert!(res.modified().is_ok());
    }

    #[test]
    fn directory_resource_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let path = dir.path().join("scratch");
        fs::create_dir(&path).unwrap();
        let id = ResourceId::new();
        let core = test_core(&dir, &id);
        let res = DirectoryResource::new(id.clone(), &path, core);
        assert!(res.exists());
        assert_eq!(res.path(), path.as_path());
        assert!(res.modified().is_ok());
    }
}
