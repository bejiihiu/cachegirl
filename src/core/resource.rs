//! Core resource identity and metadata.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use crate::core::error::ConfigError;
use crate::core::lifecycle::ResourceState;

/// Unique identifier of a registered resource.
///
/// Opaque string; generate one with [`ResourceId::new`].
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ResourceId(String);

impl ResourceId {
    /// Generate a fresh unique id.
    ///
    /// Std-only generator: wall-clock nanos, a process-wide counter and
    /// the pid. Ids are registry keys, not security tokens, so
    /// cryptographic randomness is not required.
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("res-{nanos:x}-{tick:x}-{}", std::process::id()))
    }

    /// Borrow the raw id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ResourceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ResourceId {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(ConfigError::Invalid("resource id must not be empty".into()));
        }
        Ok(Self(s.to_owned()))
    }
}

/// What kind of resource this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResourceKind {
    /// A temporary file.
    File,
    /// A temporary directory.
    Directory,
    /// An in-memory buffer.
    Buffer,
    /// A cached computation result.
    Cache,
    /// A (de)serialization intermediate.
    Serialization,
    /// A resource tied to a runtime session or operation.
    Runtime,
    /// A user-defined resource category.
    Custom,
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ResourceKind::File => "file",
            ResourceKind::Directory => "directory",
            ResourceKind::Buffer => "buffer",
            ResourceKind::Cache => "cache",
            ResourceKind::Serialization => "serialization",
            ResourceKind::Runtime => "runtime",
            ResourceKind::Custom => "custom",
        };
        f.write_str(s)
    }
}

/// Bookkeeping record for a single managed resource.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResourceMeta {
    /// Unique id assigned at registration time.
    pub id: ResourceId,
    /// Human-friendly name given at creation.
    pub name: String,
    /// Namespace the resource belongs to.
    pub namespace: String,
    /// Resource category.
    pub kind: ResourceKind,
    /// Optional owner tag, e.g. a component or request id.
    pub owner: Option<String>,
    /// When the resource was created.
    pub created_at: SystemTime,
    /// Last time the resource was accessed.
    pub last_accessed: SystemTime,
    /// Tracked size in bytes, if known.
    pub size_bytes: u64,
    /// Current lifecycle state.
    pub state: ResourceState,
    /// Free-form user metadata.
    pub metadata: HashMap<String, String>,
}

impl ResourceMeta {
    /// Build a fresh record in `Created` state.
    pub fn new(name: impl Into<String>, namespace: impl Into<String>, kind: ResourceKind) -> Self {
        let now = SystemTime::now();
        Self {
            id: ResourceId::new(),
            name: name.into(),
            namespace: namespace.into(),
            kind,
            owner: None,
            created_at: now,
            last_accessed: now,
            size_bytes: 0,
            state: ResourceState::Created,
            metadata: HashMap::new(),
        }
    }

    /// Bump the access timestamp, e.g. on every read.
    pub fn record_access(&mut self) {
        self.last_accessed = SystemTime::now();
    }
}
