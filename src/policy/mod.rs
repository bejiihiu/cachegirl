//! Cleanup policies: when and in what order resources may be reclaimed.

mod cleanup;
mod quota;
mod ttl;

pub use cleanup::{CleanupPolicy, CleanupPolicyBuilder, Priority};
pub use quota::Quota;
pub use ttl::Ttl;
