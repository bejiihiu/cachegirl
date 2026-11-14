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
    /// A blocking task offloaded to an async runtime panicked.
    #[error("a blocking task panicked: {0}")]
    BlockingTaskPanic(String),
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
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn render<E: std::error::Error>(err: &E) -> String {
        let rendered = err.to_string();
        assert!(!rendered.is_empty(), "an error rendered as an empty string");
        rendered
    }

    #[test]
    fn resource_creation_errors_render() {
        let io = render(&ResourceCreationError::Io(std::io::Error::other("boom")));
        assert!(io.contains("boom"));
        let name = render(&ResourceCreationError::InvalidName("bad/name".into()));
        assert!(name.contains("bad/name"));
        let unsupported = render(&ResourceCreationError::UnsupportedKind {
            backend: "memory".into(),
            kind: "file".into(),
        });
        assert!(unsupported.contains("memory"));
        assert!(unsupported.contains("file"));
        let backend = render(&ResourceCreationError::Backend {
            backend: "filesystem".into(),
            source: BackendError::Rejected {
                backend: "filesystem".into(),
                reason: "no space left".into(),
            },
        });
        assert!(backend.contains("filesystem"));
        assert!(backend.contains("no space left"));
    }

    #[test]
    fn backend_errors_render() {
        let unavailable = render(&BackendError::Unavailable {
            backend: "filesystem".into(),
            reason: "read-only root".into(),
        });
        assert!(unavailable.contains("read-only root"));
        let io = render(&BackendError::Io {
            backend: "filesystem".into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        });
        assert!(io.contains("filesystem"));
        assert!(io.contains("denied"));
        let rejected = render(&BackendError::Rejected {
            backend: "cache".into(),
            reason: "duplicate key".into(),
        });
        assert!(rejected.contains("duplicate key"));
        let escape = render(&BackendError::PathEscape(PathBuf::from("/etc/passwd")));
        assert!(escape.contains("/etc/passwd"));
    }

    #[test]
    fn quota_errors_render() {
        let size = render(&QuotaError::SizeExceeded {
            limit: 100,
            requested: 101,
        });
        assert!(size.contains("100"));
        assert!(size.contains("101"));
        let count = render(&QuotaError::CountExceeded { limit: 7 });
        assert!(count.contains('7'));
        let ns = render(&QuotaError::Namespace {
            namespace: "protocol".into(),
            reason: "count".into(),
        });
        assert!(ns.contains("protocol"));
    }

    #[test]
    fn access_errors_render() {
        let denied = render(&AccessError::PermissionDenied("root owned".into()));
        assert!(denied.contains("root owned"));
        let outside = render(&AccessError::OutsideRoot {
            path: PathBuf::from("/tmp/escape"),
            root: PathBuf::from("/tmp/root"),
        });
        assert!(outside.contains("/tmp/escape"));
        assert!(outside.contains("/tmp/root"));
        let symlink = render(&AccessError::SymlinkRejected(PathBuf::from("/tmp/link")));
        assert!(symlink.contains("/tmp/link"));
        let name = render(&AccessError::UnsafeName("a/b".into()));
        assert!(name.contains("a/b"));
    }

    #[test]
    fn registry_errors_render() {
        let id = ResourceId::new();
        let not_found = render(&RegistryError::NotFound(id.clone()));
        assert!(not_found.contains(id.as_str()));
        let already = render(&RegistryError::AlreadyRegistered(id.clone()));
        assert!(already.contains(id.as_str()));
        let transition = render(&RegistryError::InvalidTransition {
            from: ResourceState::Created,
            to: ResourceState::Destroyed,
        });
        assert!(transition.contains("Created"));
        assert!(transition.contains("Destroyed"));
        let poisoned = render(&RegistryError::Poisoned);
        assert!(poisoned.contains("poisoned"));
    }

    #[test]
    fn cleanup_errors_render() {
        let id = ResourceId::new();
        let removal = render(&CleanupError::Removal {
            id: id.clone(),
            reason: "file is locked".into(),
        });
        assert!(removal.contains(id.as_str()));
        assert!(removal.contains("file is locked"));
        let io = render(&CleanupError::Io(std::io::Error::other("disk gone")));
        assert!(io.contains("disk gone"));
        let leased = render(&CleanupError::Leased(id.clone()));
        assert!(leased.contains(id.as_str()));
    }

    #[test]
    fn recovery_errors_render() {
        let inspect = render(&RecoveryError::Inspect {
            path: PathBuf::from("/var/tmp/instance-1"),
            source: std::io::Error::other("unreadable"),
        });
        assert!(inspect.contains("instance-1"));
        assert!(inspect.contains("unreadable"));
        let corrupt = render(&RecoveryError::CorruptLock {
            path: PathBuf::from("/var/tmp/instance-1/.cachegirl.lock"),
            reason: "no pid field".into(),
        });
        assert!(corrupt.contains("no pid field"));
        let live = render(&RecoveryError::LiveInstance {
            path: PathBuf::from("/var/tmp/instance-2"),
            pid: 4242,
        });
        assert!(live.contains("4242"));
        assert!(live.contains("instance-2"));
    }

    #[test]
    fn concurrency_errors_render() {
        let id = ResourceId::new();
        let locked = render(&ConcurrencyError::Locked(id.clone()));
        assert!(locked.contains(id.as_str()));
        let limit = render(&ConcurrencyError::LeaseLimit(id.clone()));
        assert!(limit.contains(id.as_str()));
        let destroying = render(&ConcurrencyError::Destroying(id.clone()));
        assert!(destroying.contains(id.as_str()));
        assert!(!render(&ConcurrencyError::Cancelled).is_empty());
        let panic = render(&ConcurrencyError::BlockingTaskPanic("worker died".into()));
        assert!(panic.contains("worker died"));
        assert!(!render(&ConcurrencyError::ManagerClosed).is_empty());
        assert!(!render(&ConcurrencyError::Poisoned).is_empty());
    }

    #[test]
    fn config_errors_render() {
        let root = render(&ConfigError::RootMissing(PathBuf::from("/no/such/root")));
        assert!(root.contains("/no/such/root"));
        let create = render(&ConfigError::RootCreate {
            path: PathBuf::from("/cannot/create"),
            source: std::io::Error::other("permission denied"),
        });
        assert!(create.contains("/cannot/create"));
        assert!(create.contains("permission denied"));
        let instance = render(&ConfigError::InvalidInstanceId("bad id".into()));
        assert!(instance.contains("bad id"));
        let namespace = render(&ConfigError::InvalidNamespaceName("a/b".into()));
        assert!(namespace.contains("a/b"));
        let invalid = render(&ConfigError::Invalid("unsupported option".into()));
        assert!(invalid.contains("unsupported option"));
    }

    #[test]
    fn top_level_wrappers_are_transparent() {
        // the outer enum must add no formatting of its own
        let cases: Vec<(Error, String)> = vec![
            (
                ResourceCreationError::InvalidName("x".into()).into(),
                ResourceCreationError::InvalidName("x".into()).to_string(),
            ),
            (
                BackendError::PathEscape(PathBuf::from("/e")).into(),
                BackendError::PathEscape(PathBuf::from("/e")).to_string(),
            ),
            (
                QuotaError::CountExceeded { limit: 1 }.into(),
                QuotaError::CountExceeded { limit: 1 }.to_string(),
            ),
            (
                AccessError::UnsafeName("n".into()).into(),
                AccessError::UnsafeName("n".into()).to_string(),
            ),
            (
                RegistryError::Poisoned.into(),
                RegistryError::Poisoned.to_string(),
            ),
            (
                CleanupError::Io(std::io::Error::other("e")).into(),
                CleanupError::Io(std::io::Error::other("e")).to_string(),
            ),
            (
                RecoveryError::LiveInstance {
                    path: PathBuf::from("/p"),
                    pid: 1,
                }
                .into(),
                RecoveryError::LiveInstance {
                    path: PathBuf::from("/p"),
                    pid: 1,
                }
                .to_string(),
            ),
            (
                ConcurrencyError::ManagerClosed.into(),
                ConcurrencyError::ManagerClosed.to_string(),
            ),
            (
                ConfigError::Invalid("c".into()).into(),
                ConfigError::Invalid("c".into()).to_string(),
            ),
        ];
        for (wrapper, inner) in cases {
            assert_eq!(wrapper.to_string(), inner);
        }
    }

    #[test]
    fn io_backed_variants_expose_a_source() {
        use std::error::Error as _;
        let err = CleanupError::Io(std::io::Error::other("boom"));
        assert!(err.source().is_some());
        let err = BackendError::Io {
            backend: "b".into(),
            source: std::io::Error::other("boom"),
        };
        assert!(err.source().is_some());
        let err = ResourceCreationError::Io(std::io::Error::other("boom"));
        assert!(err.source().is_some());
        let err = RecoveryError::Inspect {
            path: PathBuf::from("/p"),
            source: std::io::Error::other("boom"),
        };
        assert!(err.source().is_some());
        let err = ConfigError::RootCreate {
            path: PathBuf::from("/p"),
            source: std::io::Error::other("boom"),
        };
        assert!(err.source().is_some());
    }
}
