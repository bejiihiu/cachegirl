//! Time-to-live modes.

use core::time::Duration;
use std::time::SystemTime;

/// How a resource's lifetime is measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Ttl {
    /// Fixed lifetime counted from creation.
    Fixed(Duration),
    /// Sliding lifetime counted from the last access.
    Idle(Duration),
}

impl Ttl {
    /// Evaluate whether the resource is expired at `now`.
    pub fn is_expired(
        &self,
        created_at: SystemTime,
        last_accessed: SystemTime,
        now: SystemTime,
    ) -> bool {
        let expired = |since: SystemTime, ttl: Duration| {
            now.duration_since(since)
                .map(|elapsed| elapsed >= ttl)
                .unwrap_or(false)
        };
        match self {
            Ttl::Fixed(ttl) => expired(created_at, *ttl),
            Ttl::Idle(ttl) => expired(last_accessed, *ttl),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_ttl_uses_creation_time() {
        let created = SystemTime::now() - Duration::from_secs(3600);
        let accessed = SystemTime::now(); // recently touched, still expired
        let now = SystemTime::now();
        assert!(Ttl::Fixed(Duration::from_secs(1800)).is_expired(created, accessed, now));
        assert!(!Ttl::Fixed(Duration::from_secs(7200)).is_expired(created, accessed, now));
    }

    #[test]
    fn idle_ttl_uses_last_access() {
        let created = SystemTime::now() - Duration::from_secs(3600);
        let accessed = SystemTime::now() - Duration::from_secs(10);
        let now = SystemTime::now();
        assert!(!Ttl::Idle(Duration::from_secs(60)).is_expired(created, accessed, now));
        assert!(Ttl::Idle(Duration::from_secs(5)).is_expired(created, accessed, now));
    }
}
