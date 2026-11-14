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
}
