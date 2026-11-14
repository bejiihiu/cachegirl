//! Typed errors shared by every part of the crate.
//!
//! Errors are split by category so callers can match on exactly the
//! failure they care about instead of poking at strings.

use std::path::PathBuf;

use thiserror::Error;

use crate::core::lifecycle::ResourceState;
use crate::core::resource::ResourceId;

/// Failures while creating a resource.
#[derive(Error, Debug)]
pub enum ResourceCreationError {
    /// The resource name failed validation.
    #[error("invalid resource name `{0}`")]
    InvalidName(String),
    /// The backend does not support the requested resource category.
    #[error("backend `{backend}` does not support resource kind `{kind}`")]
    UnsupportedKind {
        /// Backend that rejected the request.
        backend: String,
        /// Requested resource kind.
        kind: String,
    },
    /// The backend itself failed to materialize the resource.
    #[error("backend `{backend}` failed to create the resource: {source}")]
    Backend {
        /// Backend that failed.
        backend: String,
        /// Underlying backend error.
        #[source]
        source: BackendError,
    },
    /// An I/O error surfaced during creation.
    #[error("i/o error while creating the resource: {0}")]
    Io(#[from] std::io::Error),
}

/// Failures reported by storage backends.
#[derive(Error, Debug)]
pub enum BackendError {
    /// The backend is not available in the current environment.
    #[error("backend `{backend}` is unavailable: {reason}")]
    Unavailable {
        /// Backend name.
        backend: String,
        /// Why it is unavailable.
        reason: String,
    },
    /// An I/O error inside the backend.
    #[error("backend `{backend}` i/o error: {source}")]
    Io {
        /// Backend name.
        backend: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The backend rejected the operation on policy grounds.
    #[error("backend `{backend}` rejected the operation: {reason}")]
    Rejected {
        /// Backend name.
        backend: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A path would escape the backend's root directory.
    #[error("path `{0}` escapes the backend root")]
    PathEscape(PathBuf),
}

/// Quota violations: size, count, or per-namespace limits.
#[derive(Error, Debug)]
pub enum QuotaError {
    /// The byte quota would be exceeded.
    #[error("size quota exceeded: limit is {limit} bytes, requested {requested} bytes")]
    SizeExceeded {
        /// Configured limit in bytes.
        limit: u64,
        /// Bytes that would have been required.
        requested: u64,
    },
    /// The resource count quota would be exceeded.
    #[error("resource count quota exceeded: limit is {limit} resources")]
    CountExceeded {
        /// Configured resource count limit.
        limit: u64,
    },
    /// A namespace-specific quota was exceeded.
    #[error("namespace `{namespace}` exceeded its quota: {reason}")]
    Namespace {
        /// Namespace that hit the limit.
        namespace: String,
        /// What exactly was exceeded.
        reason: String,
    },
}

/// Access and safety violations: permissions, roots, symlinks, names.
#[derive(Error, Debug)]
pub enum AccessError {
    /// The operating system denied access.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The path lies outside the allowed root directory.
    #[error("path `{path}` is outside the allowed root `{root}`")]
    OutsideRoot {
        /// Offending path.
        path: PathBuf,
        /// Configured root.
        root: PathBuf,
    },
    /// A symlink was rejected by policy.
    #[error("symlink rejected at `{0}`")]
    SymlinkRejected(PathBuf),
    /// A resource name failed safety validation.
    #[error("unsafe resource name `{0}`")]
    UnsafeName(String),
}

/// Registry bookkeeping failures: missing entries, state machine misuse.
#[derive(Error, Debug)]
pub enum RegistryError {
    /// The resource is not present in the registry.
    #[error("resource `{0}` is not registered")]
    NotFound(ResourceId),
    /// The resource is already present in the registry.
    #[error("resource `{0}` is already registered")]
    AlreadyRegistered(ResourceId),
    /// The requested lifecycle transition is not allowed.
    #[error("invalid state transition from `{from:?}` to `{to:?}`")]
    InvalidTransition {
        /// Current state.
        from: ResourceState,
        /// Requested next state.
        to: ResourceState,
    },
    /// The registry mutex was poisoned by a panic.
    #[error("registry lock was poisoned")]
    Poisoned,
}

/// Failures while cleaning up resources.
///
/// One failed resource never stops the rest of a cleanup pass; failures
/// are collected and reported instead.
#[derive(Error, Debug)]
pub enum CleanupError {
    /// A resource could not be removed.
    #[error("failed to remove resource `{id}`: {reason}")]
    Removal {
        /// Resource that failed to be removed.
        id: ResourceId,
        /// Why removal failed.
        reason: String,
    },
    /// An I/O error surfaced during cleanup.
    #[error("i/o error during cleanup: {0}")]
    Io(#[from] std::io::Error),
    /// The resource is still leased and must not be deleted.
    #[error("resource `{0}` is still leased and cannot be removed")]
    Leased(ResourceId),
}

/// Failures while recovering from a crash or an unclean shutdown.
#[derive(Error, Debug)]
pub enum RecoveryError {
    /// An instance directory could not be inspected.
    #[error("failed to inspect instance directory `{path}`: {source}")]
    Inspect {
        /// Directory that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A lock file could not be parsed.
    #[error("lock file `{path}` is corrupt: {reason}")]
    CorruptLock {
        /// Lock file path.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },
    /// Recovery would remove data owned by a live process.
    #[error("refusing to remove `{path}`: it belongs to a live process (pid {pid})")]
    LiveInstance {
        /// Directory in question.
        path: PathBuf,
        /// Owning process id.
        pid: u32,
    },
}

/// Concurrency violations: locked resources, lease limits, cancellation.
#[derive(Error, Debug)]
pub enum ConcurrencyError {
    /// The resource is locked by another operation.
    #[error("resource `{0}` is locked by another operation")]
    Locked(ResourceId),
    /// The lease limit for the resource was reached.
    #[error("lease limit reached for resource `{0}`")]
    LeaseLimit(ResourceId),
    /// The resource is being destroyed right now; new leases are refused.
    #[error("resource `{0}` is being destroyed and cannot accept leases")]
    Destroying(ResourceId),
    /// The operation was cancelled.
    #[error("the operation was cancelled")]
    Cancelled,
    /// The manager has already been shut down.
    #[error("the manager has been shut down")]
    ManagerClosed,
    /// A synchronization primitive was poisoned by a panic.
    #[error("synchronization primitive was poisoned")]
    Poisoned,
}

/// Misconfiguration detected while building or running a manager.
#[derive(Error, Debug)]
pub enum ConfigError {
    /// The root directory does not exist and auto-creation is disabled.
    #[error("root directory `{0}` does not exist and `create_if_missing` is disabled")]
    RootMissing(PathBuf),
    /// The root directory could not be created.
    #[error("failed to create root directory `{path}`: {source}")]
    RootCreate {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The instance id failed validation.
    #[error("invalid instance id `{0}`")]
    InvalidInstanceId(String),
    /// The namespace name failed validation.
    #[error("invalid namespace name `{0}`")]
    InvalidNamespaceName(String),
    /// Another configuration problem was found.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Top-level error type for the whole crate.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Resource creation failed.
    #[error(transparent)]
    Create(#[from] ResourceCreationError),
    /// A storage backend failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A quota was exceeded.
    #[error(transparent)]
    Quota(#[from] QuotaError),
    /// An access or safety check failed.
    #[error(transparent)]
    Access(#[from] AccessError),
    /// Registry bookkeeping failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Cleanup failed.
    #[error(transparent)]
    Cleanup(#[from] CleanupError),
    /// Crash recovery failed.
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// A concurrency invariant was violated.
    #[error(transparent)]
    Concurrency(#[from] ConcurrencyError),
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
