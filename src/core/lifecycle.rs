//! Resource lifecycle state machine.
//!
//! Every managed resource moves through a fixed set of states. Illegal
//! transitions are rejected instead of silently corrupting bookkeeping.

use crate::core::error::RegistryError;

/// Lifecycle state of a single resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResourceState {
    /// Reserved placeholder before the backend materializes the resource.
    Uninitialized,
    /// Created and registered, but not handed out yet.
    Created,
    /// In active use by the application.
    Active,
    /// Temporarily locked by an exclusive operation.
    Locked,
    /// Past its TTL; eligible for cleanup.
    Expired,
    /// Queued to be destroyed by a cleanup pass.
    PendingCleanup,
    /// Reclaimed by cleanup. Terminal.
    Destroyed,
    /// Cleanup failed; kept for inspection or retry.
    FailedCleanup,
    /// Marked invalid, e.g. after an integrity check failed.
    Invalid,
    /// Being rebuilt after a crash or interrupted operation.
    Recovering,
}

impl ResourceState {
    /// Check whether a transition to `next` is legal from this state.
    pub fn can_transition_to(self, next: ResourceState) -> bool {
        use ResourceState::*;
        matches!(
            (self, next),
            (Uninitialized, Created | Invalid)
                | (
                    Created,
                    Active | Locked | Expired | PendingCleanup | Invalid
                )
                | (Active, Locked | Expired | PendingCleanup | Invalid)
                | (Locked, Active | Expired | PendingCleanup | Invalid)
                | (Expired, PendingCleanup)
                | (PendingCleanup, Destroyed | FailedCleanup)
                | (FailedCleanup, PendingCleanup | Invalid)
                | (Invalid, PendingCleanup)
                | (Recovering, Active | FailedCleanup | Invalid)
                | (Created | Active | Locked, Recovering)
        )
    }

    /// Attempt a transition, returning an error for illegal moves.
    pub fn try_transition(self, next: ResourceState) -> Result<ResourceState, RegistryError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(RegistryError::InvalidTransition {
                from: self,
                to: next,
            })
        }
    }

    /// True for states a cleanup pass may act on.
    pub fn is_cleanup_eligible(self) -> bool {
        matches!(self, ResourceState::Expired | ResourceState::PendingCleanup)
    }

    /// True for the terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, ResourceState::Destroyed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_is_legal() {
        let s = ResourceState::Uninitialized;
        let s = s.try_transition(ResourceState::Created).unwrap();
        let s = s.try_transition(ResourceState::Active).unwrap();
        let s = s.try_transition(ResourceState::Expired).unwrap();
        let s = s.try_transition(ResourceState::PendingCleanup).unwrap();
        assert!(s.try_transition(ResourceState::Destroyed).is_ok());
    }

    #[test]
    fn illegal_transition_is_rejected() {
        assert!(
            ResourceState::Destroyed
                .try_transition(ResourceState::Active)
                .is_err()
        );
        assert!(
            ResourceState::Expired
                .try_transition(ResourceState::Active)
                .is_err()
        );
    }

    #[test]
    fn cleanup_eligibility() {
        assert!(ResourceState::Expired.is_cleanup_eligible());
        assert!(ResourceState::PendingCleanup.is_cleanup_eligible());
        assert!(!ResourceState::Active.is_cleanup_eligible());
    }
}
