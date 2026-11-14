//! Storage backends and how they plug into the manager.
//!
//! A backend is the physical side of a resource: it knows how to
//! materialize one and how to make it disappear again. The manager
//! never hard-codes a backend; it talks to the [`ResourceBackend`]
//! contract, which keeps the core usable without a filesystem.

mod custom;
#[cfg(feature = "filesystem")]
mod filesystem;
#[cfg(feature = "memory")]
mod memory;

pub use custom::BackendRegistry;
#[cfg(feature = "filesystem")]
pub use filesystem::FilesystemBackend;
#[cfg(feature = "memory")]
pub use memory::MemoryBackend;

use std::collections::HashMap;

use crate::core::error::Result;
use crate::core::resource::{ResourceKind, ResourceMeta};

/// Everything a backend needs to know to create a resource.
#[derive(Clone, Debug)]
pub struct ResourceRequest {
    /// Human-friendly resource name.
    pub name: String,
    /// Namespace the resource belongs to.
    pub namespace: String,
    /// Category of resource.
    pub kind: ResourceKind,
    /// Expected size in bytes, if known. Backends may enforce it.
    pub size_hint: Option<u64>,
    /// Free-form user metadata.
    pub metadata: HashMap<String, String>,
}

impl ResourceRequest {
    /// Minimal request: name, namespace, kind.
    pub fn new(name: impl Into<String>, namespace: impl Into<String>, kind: ResourceKind) -> Self {
        Self {
            name: name.into(),
            namespace: namespace.into(),
            kind,
            size_hint: None,
            metadata: HashMap::new(),
        }
    }

    /// Attach a size hint.
    pub fn with_size_hint(mut self, bytes: u64) -> Self {
        self.size_hint = Some(bytes);
        self
    }
}

/// Contract between the manager and a storage backend.
///
/// Object-safe on purpose so backends can be registered and swapped at
/// runtime. Implementations must confine all physical operations to
/// their own root and must never follow paths outside of it.
pub trait ResourceBackend: Send + Sync {
    /// Stable backend name, used for diagnostics and registry lookups.
    fn name(&self) -> &str;

    /// Create a resource described by `request`.
    fn create(&self, request: &ResourceRequest) -> Result<ResourceMeta>;

    /// Destroy a previously created resource.
    ///
    /// Releasing a resource twice must succeed silently — cleanup is
    /// allowed to run more than once.
    fn release(&self, meta: &ResourceMeta) -> Result<()>;
}
