//! Async API over the synchronous core.
//!
//! Everything in here is a thin, runtime-honest façade: the sync core
//! stays the single implementation, and this module forwards to it.
//! No architecture is duplicated, no behavior forks — just await points.
//!
//! ## Execution model
//!
//! What happens inside each `async fn` depends on how the crate was
//! built:
//!
//! - **`async` feature alone** — no runtime is required anywhere. Every
//!   operation runs on the calling thread, exactly like the sync API,
//!   just behind an `async fn` signature so it composes with any
//!   executor you already have.
//! - **`tokio` feature, ambient Tokio runtime** — operations that may
//!   block (filesystem access, cleanup sweeps, startup recovery, and any
//!   call into a custom backend) are offloaded to
//!   `tokio::task::spawn_blocking`, so the executor is never blocked.
//!   Memory-only operations (buffers, serialization payloads, cache
//!   entries, registry bookkeeping) never leave the current thread.
//! - **`tokio` feature, no ambient runtime** — falls back to running on
//!   the calling thread, i.e. exactly the `async`-feature behavior.
//!   Nothing panics, nothing is faked.
//!
//! ## Resource handles stay synchronous
//!
//! [`FileResource`](crate::FileResource) and friends are deliberately
//! not wrapped: `Drop` cannot be async, and lease acquisition is atomic
//! bookkeeping in the lease book. The one blocking call on a handle is
//! `FileResource::len()`/`modified()` (filesystem metadata) — in async
//! code, prefer `AsyncNamespace::refresh_size` or a blocking-pool call.
//!
//! Note that dropping an `Owned` handle destroys the resource inline,
//! which touches the filesystem. If that matters to your executor, drop
//! the handle inside `spawn_blocking` or switch the resource to
//! `Managed` ownership and release it explicitly.
//!
//! ## Example
//!
//! One of the ways to drive it — with a Tokio runtime:
//!
//! ```rust
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), cachegirl::Error> {
//! use cachegirl::AsyncTempManager;
//!
//! let dir = std::env::temp_dir().join(format!("cachegirl-async-doc-{}", std::process::id()));
//! let manager = AsyncTempManager::builder()
//!     .root(&dir)
//!     .create_if_missing(true)
//!     .build()
//!     .await?;
//!
//! let ns = manager.namespace("protocol").await?;
//! let file = ns.create_file("packet-dump.bin").await?;
//! assert!(file.path().exists());
//!
//! let id = file.id().clone();
//! assert!(ns.release(&id).await?);
//!
//! manager.shutdown().await?;
//! let _ = std::fs::remove_dir_all(&dir);
//! # Ok(())
//! # }
//! ```
//!
//! ## Using it without a runtime
//!
//! With the `async` feature alone there is nothing to set up:
//!
//! ```rust
//! use cachegirl::AsyncTempManager;
//!
//! async fn ship() -> Result<(), cachegirl::Error> {
//!     let dir = std::env::temp_dir().join(format!("cachegirl-async-doc2-{}", std::process::id()));
//!     let manager = AsyncTempManager::builder().root(&dir).build().await?;
//!     let ns = manager.namespace("protocol").await?;
//!     assert_eq!(ns.name(), "protocol");
//!     assert_eq!(manager.namespaces().await?.len(), 1);
//!     manager.shutdown().await?;
//!     let _ = std::fs::remove_dir_all(&dir);
//!     Ok(())
//! }
//! // driving `ship()` is the caller's job: any executor, any runtime,
//! // or a hand-rolled one — the library never picks for you
//! ```

mod manager;
mod namespace;

pub use manager::{AsyncTempManager, AsyncTempManagerBuilder};
pub use namespace::AsyncNamespace;

use crate::core::error::Result;

/// Run `f` on Tokio's blocking pool when a runtime is around, inline otherwise.
///
/// The fallback is deliberate: without an ambient runtime there is
/// nothing to offload *to*, and failing (or panicking) would punish the
/// embedding application for a perfectly reasonable call site. Inline
/// execution is exactly what the `async`-feature build does everywhere.
#[cfg(feature = "tokio")]
pub(crate) async fn offload<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => {
            tokio::task::spawn_blocking(f)
                .await
                .map_err(|join| -> crate::core::error::Error {
                    match join.is_panic() {
                        true => crate::core::error::ConcurrencyError::BlockingTaskPanic(
                            join.to_string(),
                        )
                        .into(),
                        false => crate::core::error::ConcurrencyError::Cancelled.into(),
                    }
                })?
        }
        Err(_) => f(),
    }
}

/// Runtime-agnostic build: everything runs on the calling thread.
#[cfg(not(feature = "tokio"))]
pub(crate) async fn offload<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    f()
}
