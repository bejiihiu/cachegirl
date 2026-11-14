//! Cleanup policies: when and in what order resources may be reclaimed.

mod cleanup;
mod custom;
mod quota;
mod ttl;

pub use cleanup::{CleanupPolicy, CleanupPolicyBuilder, Priority};
pub use custom::{CleanupContext, CleanupScope, CustomCleanupPolicy, PolicyDecision};
pub use quota::Quota;
pub use ttl::Ttl;
