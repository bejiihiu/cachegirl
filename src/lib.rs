//! # cachegirl
//!
//! `cachegirl` is a library for centralized management of temporary
//! resources in Rust applications: temporary files, directories,
//! in-memory buffers, cached computation results, and anything else
//! with a limited lifetime.
//!
//! Instead of scattering ad-hoc `tempdir` calls and manual cleanup
//! logic across your codebase, `cachegirl` gives you one registry of
//! everything temporary, with namespaces, quotas, leases, cleanup
//! policies, and crash recovery — without dictating your architecture.
//!
//! ## Design principles
//!
//! - **No forced runtime.** The core never requires an async runtime
//!   or background threads. You call cleanup when it makes sense.
//! - **Explicit ownership.** Every resource is created with an
//!   [`Ownership`] mode. Dropping the last owning handle destroys an
//!   `Owned`/`Shared` resource for real; `Managed` and `Leased`
//!   resources live until release or cleanup. Leases protect a resource
//!   while it is in use: `handle.acquire()` hands out an RAII lease and
//!   nothing is physically deleted while one is held.
//! - **No forced layout.** You choose the root directory; the library
//!   never imposes `/tmp` or any other location.
//! - **No global state.** Independent managers can coexist in one
//!   process.
//! - **Typed errors.** Every failure category has its own error type.
//! - **Optional integrations.** Filesystem, memory, cache, async,
//!   `serde`, `metrics`, `tracing`, and Tokio support are feature-gated.
//!
//! ## Example
//!
//! ```rust
//! use cachegirl::TempManager;
//!
//! let dir = std::env::temp_dir().join(format!("cachegirl-doc-{}", std::process::id()));
//! let manager = TempManager::builder()
//!     .root(&dir)
//!     .create_if_missing(true)
//!     .build()?;
//!
//! let protocol = manager.namespace("protocol")?;
//! let file = protocol.create_file("packet-dump.bin")?;
//! assert!(file.path().exists());
//! protocol.release(file.id())?;
//!
//! let report = manager.cleanup_expired()?;
//! assert_eq!(report.removed, 0);
//!
//! manager.shutdown()?;
//! let _ = std::fs::remove_dir_all(&dir);
//! # Ok::<(), cachegirl::Error>(())
//! ```
//!
//! ## Repository
//! <https://github.com/bejiihiu/cachegirl>

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(test)]
mod testsupport;

pub mod backend;
pub mod cleanup;
pub mod core;
pub mod namespace;
pub mod observability;
pub mod ownership;
pub mod policy;
pub mod recovery;
pub mod resource;

pub use cleanup::{CleanupEngine, CleanupReport, MAX_CLEANUP_ATTEMPTS, RemovalReasons};
pub use core::error::{
    AccessError, BackendError, CleanupError, ConcurrencyError, ConfigError, Error, QuotaError,
    RecoveryError, RegistryError, ResourceCreationError, Result,
};
pub use core::lifecycle::ResourceState;
pub use core::manager::{ManagerConfig, SymlinkPolicy, TempManager, TempManagerBuilder};
pub use core::registry::Registry;
pub use core::resource::{Ownership, ResourceId, ResourceKind, ResourceMeta};
pub use namespace::{CreateOptions, Namespace, NamespaceQuota};
#[cfg(feature = "tracing")]
pub use observability::TracingSink;
pub use observability::{Event, EventSink, NullSink, Statistics, StatisticsCollector, VecSink};
pub use ownership::{Lease, LeaseBook, LockGuard, LockSet};
pub use policy::{
    CleanupContext, CleanupPolicy, CleanupPolicyBuilder, CleanupScope, CustomCleanupPolicy,
    PolicyDecision, Priority, Quota, Ttl,
};
pub use recovery::{
    DEFAULT_STALE_INSTANCE_AFTER, RecoveryAction, RecoveryEntry, RecoveryManager, RecoveryPolicy,
    RecoveryReport,
};
#[cfg(feature = "cache")]
pub use resource::CacheEntryResource;
#[cfg(feature = "memory")]
pub use resource::{BufferResource, RuntimeResource, SerializationResource};
pub use resource::{CustomResource, ResourceHandle};
#[cfg(feature = "filesystem")]
pub use resource::{DirectoryResource, FileResource};
