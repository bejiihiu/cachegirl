//! Concrete resource types and the RAII handle.

#[cfg(feature = "memory")]
mod buffer;
#[cfg(feature = "cache")]
mod cache;
#[cfg(feature = "filesystem")]
mod file;

mod handle;

#[cfg(feature = "memory")]
pub use buffer::Buffer;
#[cfg(feature = "cache")]
pub use cache::CacheEntry;
#[cfg(feature = "filesystem")]
pub use file::FileResource;
pub use handle::ResourceHandle;
