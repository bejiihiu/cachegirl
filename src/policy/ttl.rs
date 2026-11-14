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

    #[test]
    fn fixed_ttl_expires_exactly_at_the_boundary() {
        // elapsed == ttl counts as expired, not one millisecond later
        let ttl = Duration::from_secs(100);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let now = created + ttl;
        assert!(Ttl::Fixed(ttl).is_expired(created, created, now));
        let just_before = created + ttl - Duration::from_millis(1);
        assert!(!Ttl::Fixed(ttl).is_expired(created, created, just_before));
    }

    #[test]
    fn idle_ttl_expires_exactly_at_the_boundary() {
        let ttl = Duration::from_secs(100);
        let accessed = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let now = accessed + ttl;
        assert!(Ttl::Idle(ttl).is_expired(accessed, accessed, now));
        let just_before = accessed + ttl - Duration::from_millis(1);
        assert!(!Ttl::Idle(ttl).is_expired(accessed, accessed, just_before));
    }

    #[test]
    fn zero_ttl_is_immediately_expired() {
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(Ttl::Fixed(Duration::ZERO).is_expired(created, created, created));
        assert!(Ttl::Idle(Duration::ZERO).is_expired(created, created, created));
    }

    #[test]
    fn future_timestamps_are_not_expired() {
        // a resource whose creation clock lies ahead of `now` must not be
        // reclaimed on a clock skew; same for a future last-access time
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(!Ttl::Fixed(Duration::from_secs(1)).is_expired(created, created, now));
        assert!(!Ttl::Idle(Duration::from_secs(1)).is_expired(now, created, now));
    }
}
