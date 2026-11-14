//! Cleanup policy: TTL, quotas, and reclamation priority in one builder.

use crate::policy::quota::Quota;
use crate::policy::ttl::Ttl;

/// How eagerly a resource should be reclaimed relative to others.
///
/// Eviction favours reclaiming lower-priority resources first: `Low` is
/// evicted before `Normal`, which is evicted before `High`, and
/// `Critical` only when everything else is gone. The derived `Ord`
/// reflects importance, so sorting ascending yields eviction order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Priority {
    /// Evicted first; for scratch data that is cheap to recreate.
    Low,
    /// Default.
    #[default]
    Normal,
    /// Evicted only after lower-priority resources are gone.
    High,
    /// Evicted last; for resources the application cannot afford to lose.
    Critical,
}

/// A complete cleanup policy for a resource or namespace.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CleanupPolicy {
    /// Lifetime rule, if any.
    pub ttl: Option<Ttl>,
    /// Aggregate quota.
    pub quota: Quota,
    /// Reclamation priority.
    pub priority: Priority,
}

impl CleanupPolicy {
    /// Start building a policy.
    pub fn builder() -> CleanupPolicyBuilder {
        CleanupPolicyBuilder::default()
    }
}

/// Fluent builder for [`CleanupPolicy`].
#[derive(Default)]
pub struct CleanupPolicyBuilder {
    ttl: Option<Ttl>,
    quota: Quota,
    priority: Priority,
}

impl CleanupPolicyBuilder {
    /// Fixed TTL counted from creation.
    pub fn ttl(mut self, ttl: std::time::Duration) -> Self {
        self.ttl = Some(Ttl::Fixed(ttl));
        self
    }

    /// Sliding TTL counted from the last access.
    pub fn idle_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.ttl = Some(Ttl::Idle(ttl));
        self
    }

    /// Overall size limit.
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.quota.max_bytes = Some(bytes);
        self
    }

    /// Overall resource count limit.
    pub fn max_resources(mut self, count: u64) -> Self {
        self.quota.max_resources = Some(count);
        self
    }

    /// Reclamation priority.
    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Finish the policy.
    pub fn build(self) -> CleanupPolicy {
        CleanupPolicy {
            ttl: self.ttl,
            quota: self.quota,
            priority: self.priority,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn builder_assembles_policy() {
        let p = CleanupPolicy::builder()
            .ttl(Duration::from_secs(3600))
            .max_size(512 * 1024 * 1024)
            .max_resources(10_000)
            .priority(Priority::High)
            .build();
        assert_eq!(p.ttl, Some(Ttl::Fixed(Duration::from_secs(3600))));
        assert_eq!(p.quota.max_bytes, Some(512 * 1024 * 1024));
        assert_eq!(p.quota.max_resources, Some(10_000));
        assert_eq!(p.priority, Priority::High);
    }

    #[test]
    fn default_is_unlimited_and_normal() {
        let p = CleanupPolicy::default();
        assert!(p.ttl.is_none());
        assert_eq!(p.quota, Quota::unlimited());
        assert_eq!(p.priority, Priority::Normal);
    }

    #[test]
    fn eviction_order_is_low_normal_high_critical() {
        // eviction scans ascending: the weakest priority goes first
        let mut order = vec![
            Priority::Critical,
            Priority::High,
            Priority::Normal,
            Priority::Low,
        ];
        order.sort();
        assert_eq!(
            order,
            vec![
                Priority::Low,
                Priority::Normal,
                Priority::High,
                Priority::Critical
            ]
        );
    }
}
