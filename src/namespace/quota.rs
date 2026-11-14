//! Per-namespace quota accounting.
//!
//! A quota is a plain limit object; enforcement happens where bytes and
//! resource counts are actually known.

use crate::core::error::QuotaError;

/// Size and count limits for a namespace (or the whole manager).
///
/// `None` means "no limit" for that dimension.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NamespaceQuota {
    /// Maximum tracked bytes.
    pub max_bytes: Option<u64>,
    /// Maximum number of live resources.
    pub max_resources: Option<u64>,
}

impl NamespaceQuota {
    /// No limits at all.
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Only a size limit.
    pub fn with_max_bytes(max_bytes: u64) -> Self {
        Self {
            max_bytes: Some(max_bytes),
            max_resources: None,
        }
    }

    /// Only a count limit.
    pub fn with_max_resources(max_resources: u64) -> Self {
        Self {
            max_bytes: None,
            max_resources: Some(max_resources),
        }
    }

    /// Both limits.
    pub fn with_limits(max_bytes: u64, max_resources: u64) -> Self {
        Self {
            max_bytes: Some(max_bytes),
            max_resources: Some(max_resources),
        }
    }

    /// Check whether adding `added_bytes` on top of current usage fits.
    pub fn check(
        &self,
        namespace: &str,
        current_bytes: u64,
        current_resources: u64,
        added_bytes: u64,
    ) -> Result<(), QuotaError> {
        if let Some(limit) = self.max_bytes {
            let projected = current_bytes.saturating_add(added_bytes);
            if projected > limit {
                return Err(QuotaError::SizeExceeded {
                    limit,
                    requested: projected,
                });
            }
        }
        if let Some(limit) = self.max_resources {
            let projected = current_resources.saturating_add(1);
            if projected > limit {
                return Err(QuotaError::CountExceeded { limit });
            }
        }
        let _ = namespace; // kept for richer diagnostics downstream
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limit_enforced() {
        let q = NamespaceQuota::with_max_bytes(100);
        assert!(q.check("ns", 90, 0, 10).is_ok());
        assert!(q.check("ns", 90, 0, 11).is_err());
    }

    #[test]
    fn count_limit_enforced() {
        let q = NamespaceQuota::with_max_resources(2);
        assert!(q.check("ns", 0, 1, 0).is_ok());
        assert!(q.check("ns", 0, 2, 0).is_err());
    }

    #[test]
    fn unlimited_never_fails() {
        let q = NamespaceQuota::unlimited();
        assert!(q.check("ns", u64::MAX, u64::MAX, u64::MAX).is_ok());
    }
}
