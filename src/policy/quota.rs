//! Aggregate quota: bytes plus resource count.

/// Combined limit enforced by the policy engine.
///
/// `None` disables the respective dimension.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Quota {
    /// Maximum tracked bytes.
    pub max_bytes: Option<u64>,
    /// Maximum number of live resources.
    pub max_resources: Option<u64>,
}

impl Quota {
    /// No limits.
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Check a candidate allocation against current usage.
    pub fn allows(&self, current_bytes: u64, current_resources: u64, added_bytes: u64) -> bool {
        let bytes_ok = self
            .max_bytes
            .is_none_or(|limit| current_bytes.saturating_add(added_bytes) <= limit);
        let count_ok = self
            .max_resources
            .is_none_or(|limit| current_resources.saturating_add(1) <= limit);
        bytes_ok && count_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_limits() {
        let q = Quota {
            max_bytes: Some(100),
            max_resources: Some(2),
        };
        assert!(q.allows(50, 1, 50));
        assert!(!q.allows(50, 1, 51)); // bytes
        assert!(!q.allows(0, 2, 0)); // count
    }

    #[test]
    fn size_allows_exactly_at_the_limit_and_no_further() {
        let q = Quota {
            max_bytes: Some(100),
            max_resources: None,
        };
        assert!(q.allows(0, 0, 100));
        assert!(!q.allows(0, 0, 101));
        // exactly full counts as fitting
        assert!(q.allows(99, 0, 1));
        assert!(!q.allows(100, 0, 1));
    }

    #[test]
    fn count_allows_exactly_up_to_the_limit() {
        let q = Quota {
            max_bytes: None,
            max_resources: Some(3),
        };
        assert!(q.allows(0, 2, 0)); // the 3rd resource fits
        assert!(!q.allows(0, 3, 0)); // the 4th does not
    }

    #[test]
    fn zero_count_limit_denies_everything() {
        let q = Quota {
            max_bytes: None,
            max_resources: Some(0),
        };
        assert!(!q.allows(0, 0, 0));
    }

    #[test]
    fn byte_projection_saturates_instead_of_wrapping() {
        // near u64::MAX the projection saturates: (MAX + 1) caps at MAX,
        // which is still over the limit; wrapping would read 0 and fit
        let q = Quota {
            max_bytes: Some(u64::MAX - 1),
            max_resources: None,
        };
        assert!(!q.allows(u64::MAX, 0, 1));
        assert!(q.allows(u64::MAX - 1, 0, 0));
    }

    #[test]
    fn each_dimension_is_independent() {
        // bytes fit but count does not, and the other way around
        let bytes_only = Quota {
            max_bytes: Some(10),
            max_resources: None,
        };
        assert!(bytes_only.allows(0, u64::MAX - 1, 10));
        let count_only = Quota {
            max_bytes: None,
            max_resources: Some(1),
        };
        assert!(!count_only.allows(0, 1, u64::MAX));
    }
}
