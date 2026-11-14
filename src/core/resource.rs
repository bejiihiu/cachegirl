//! Core resource identity and metadata.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use crate::core::error::ConfigError;
use crate::core::lifecycle::ResourceState;
use crate::policy::{Priority, Ttl};

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

/// How a resource's lifetime is tied to its handles and to cleanup policy.
///
/// Chosen at creation time through [`crate::CreateOptions::ownership`] and
/// recorded in the registry record. Leases always gate physical deletion,
/// in every mode: while at least one [`crate::Lease`] is held, the
/// resource is never destroyed, only marked `PendingCleanup`.
///
/// The same four modes apply to files, buffers, cache entries,
/// serialization intermediates, runtime markers and custom resources.
///
/// # Example
///
/// ```
/// use cachegirl::{Ownership, TempManager};
///
/// let dir = std::env::temp_dir().join(format!("cachegirl-own-{}", std::process::id()));
/// let manager = TempManager::builder()
///     .root(&dir)
///     .build()?;
/// let ns = manager.namespace("scratch")?;
///
/// // Owned: dropping the handle deletes the file from disk.
/// let file = ns.create_file_with(
///     "owned.bin",
///     cachegirl::CreateOptions::new().ownership(Ownership::Owned),
/// )?;
/// let path = file.path().to_path_buf();
/// drop(file);
/// assert!(!path.exists());
///
/// manager.shutdown()?;
/// let _ = std::fs::remove_dir_all(&dir);
/// # Ok::<(), cachegirl::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Ownership {
    /// Single-owner semantics. Dropping the last owning handle destroys
    /// the resource through its backend. If leases are still held at
    /// drop time, destruction is deferred: the record is marked
    /// `PendingCleanup` and a cleanup pass destroys it once the last
    /// lease is gone.
    Owned,
    /// Policy-driven lifetime. The resource lives until it is released
    /// explicitly, expires under a TTL, or is picked up by a cleanup
    /// pass. Dropping the handle only detaches it; the registry record
    /// and the physical resource are left alone. This is the default.
    #[default]
    Managed,
    /// Shared ownership with Arc-like semantics. Every clone of the
    /// handle is a co-owner; the resource is destroyed when the last
    /// one drops, subject to the same lease rule as `Owned`.
    Shared,
    /// Lease-centric ownership. Dropping the handle detaches it; the
    /// resource is only ever reclaimed through explicit release or a
    /// cleanup pass, and both are refused while leases are held (the
    /// resource moves to `PendingCleanup` instead of being deleted).
    Leased,
}

impl Ownership {
    /// True for modes that destroy the resource when the last handle drops.
    pub(crate) fn drop_destroys(self) -> bool {
        matches!(self, Ownership::Owned | Ownership::Shared)
    }
}

impl fmt::Display for Ownership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Ownership::Owned => "owned",
            Ownership::Managed => "managed",
            Ownership::Shared => "shared",
            Ownership::Leased => "leased",
        };
        f.write_str(s)
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
///
/// `created_at` and `last_accessed` serialize as
/// `{"secs_since_epoch": ..., "nanos_since_epoch": ...}`. `ttl`,
/// `priority`, `cleanup_attempts` and `metadata` default when absent so
/// records written by older versions still load. Unknown fields are
/// ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// How long the resource may live, if a TTL applies.
    #[cfg_attr(feature = "serde", serde(default))]
    pub ttl: Option<Ttl>,
    /// Ownership mode chosen at creation time.
    pub ownership: Ownership,
    /// Eviction priority relative to other resources.
    #[cfg_attr(feature = "serde", serde(default))]
    pub priority: Priority,
    /// How many cleanup attempts the resource has already survived.
    ///
    /// Bumped every time a destruction fails; cleanup stops retrying
    /// after [`crate::cleanup::MAX_CLEANUP_ATTEMPTS`] attempts.
    #[cfg_attr(feature = "serde", serde(default))]
    pub cleanup_attempts: u32,
    /// Free-form user metadata.
    #[cfg_attr(feature = "serde", serde(default))]
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
            ttl: None,
            ownership: Ownership::Managed,
            priority: Priority::Normal,
            cleanup_attempts: 0,
            metadata: HashMap::new(),
        }
    }

    /// Bump the access timestamp, e.g. on every read.
    pub fn record_access(&mut self) {
        self.last_accessed = SystemTime::now();
    }
}
