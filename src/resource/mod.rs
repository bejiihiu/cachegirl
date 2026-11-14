//! Concrete resource types and the RAII handle.

#[cfg(feature = "memory")]
mod buffer;
#[cfg(feature = "cache")]
pub(crate) mod cache;
mod custom;
#[cfg(feature = "filesystem")]
mod file;
#[cfg(feature = "memory")]
mod pool;
#[cfg(feature = "memory")]
mod runtime;
#[cfg(feature = "memory")]
mod serialization;

#[cfg(feature = "memory")]
pub use buffer::Buffer;
#[cfg(feature = "memory")]
pub use buffer::BufferResource;
#[cfg(feature = "cache")]
pub use cache::CacheEntry;
#[cfg(feature = "cache")]
pub use cache::CacheEntryResource;
pub use custom::CustomResource;
#[cfg(feature = "filesystem")]
pub use file::DirectoryResource;
#[cfg(feature = "filesystem")]
pub use file::FileResource;
#[cfg(feature = "memory")]
pub use pool::{BufferPool, PooledBuffer};
#[cfg(feature = "memory")]
pub use runtime::RuntimeResource;
#[cfg(feature = "memory")]
pub use serialization::SerializationResource;
