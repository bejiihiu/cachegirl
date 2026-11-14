//! A file-backed resource handle.
//!
//! The handle does not delete the file on drop — deletion is a cleanup
//! policy decision, not a destructor side effect. What it gives you is
//! cheap access to the path and live size information.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::core::resource::ResourceId;

/// Lightweight handle to a registered file resource.
#[derive(Clone, Debug)]
pub struct FileResource {
    id: ResourceId,
    path: PathBuf,
}

impl FileResource {
    /// Wrap a path with its resource id.
    pub fn new(id: ResourceId, path: impl Into<PathBuf>) -> Self {
        Self {
            id,
            path: path.into(),
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

    /// Current file size, or an I/O error if it is gone.
    pub fn len(&self) -> io::Result<u64> {
        Ok(fs::metadata(&self.path)?.len())
    }

    /// True when the file exists and is empty.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// True when the file still exists.
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Last modification time reported by the filesystem.
    pub fn modified(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.modified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip() {
        let dir = crate::testsupport::TestDir::new();
        let path = dir.path().join("x.bin");
        fs::write(&path, b"hello").unwrap();
        let res = FileResource::new(ResourceId::new(), &path);
        assert!(res.exists());
        assert_eq!(res.len().unwrap(), 5);
        assert!(res.modified().is_ok());
    }
}
