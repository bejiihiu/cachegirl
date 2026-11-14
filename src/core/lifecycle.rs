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

    // every legal (from, to) pair, pinned exhaustively: the state machine
    // is the crate's bookkeeping backbone, so silent drift here corrupts
    // everything downstream
    const LEGAL: &[(ResourceState, ResourceState)] = &[
        (ResourceState::Uninitialized, ResourceState::Created),
        (ResourceState::Uninitialized, ResourceState::Invalid),
        (ResourceState::Created, ResourceState::Active),
        (ResourceState::Created, ResourceState::Locked),
        (ResourceState::Created, ResourceState::Expired),
        (ResourceState::Created, ResourceState::PendingCleanup),
        (ResourceState::Created, ResourceState::Invalid),
        (ResourceState::Created, ResourceState::Recovering),
        (ResourceState::Active, ResourceState::Locked),
        (ResourceState::Active, ResourceState::Expired),
        (ResourceState::Active, ResourceState::PendingCleanup),
        (ResourceState::Active, ResourceState::Invalid),
        (ResourceState::Active, ResourceState::Recovering),
        (ResourceState::Locked, ResourceState::Active),
        (ResourceState::Locked, ResourceState::Expired),
        (ResourceState::Locked, ResourceState::PendingCleanup),
        (ResourceState::Locked, ResourceState::Invalid),
        (ResourceState::Locked, ResourceState::Recovering),
        (ResourceState::Expired, ResourceState::PendingCleanup),
        (ResourceState::PendingCleanup, ResourceState::Destroyed),
        (ResourceState::PendingCleanup, ResourceState::FailedCleanup),
        (ResourceState::FailedCleanup, ResourceState::PendingCleanup),
        (ResourceState::FailedCleanup, ResourceState::Invalid),
        (ResourceState::Invalid, ResourceState::PendingCleanup),
        (ResourceState::Recovering, ResourceState::Active),
        (ResourceState::Recovering, ResourceState::FailedCleanup),
        (ResourceState::Recovering, ResourceState::Invalid),
    ];

    const ALL_STATES: [ResourceState; 10] = [
        ResourceState::Uninitialized,
        ResourceState::Created,
        ResourceState::Active,
        ResourceState::Locked,
        ResourceState::Expired,
        ResourceState::PendingCleanup,
        ResourceState::Destroyed,
        ResourceState::FailedCleanup,
        ResourceState::Invalid,
        ResourceState::Recovering,
    ];

    #[test]
    fn exhaustive_transition_table() {
        for from in ALL_STATES {
            for to in ALL_STATES {
                let expected = LEGAL.contains(&(from, to));
                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "can_transition_to({from:?} -> {to:?}) flipped"
                );
            }
        }
    }

    #[test]
    fn try_transition_reports_both_states_in_the_error() {
        let err = ResourceState::Destroyed
            .try_transition(ResourceState::Active)
            .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("Destroyed"));
        assert!(rendered.contains("Active"));
        // terminal means terminal: no way back out of Destroyed
        for to in ALL_STATES {
            assert!(!ResourceState::Destroyed.can_transition_to(to));
        }
    }

    #[test]
    fn self_transitions_are_all_rejected() {
        for state in ALL_STATES {
            assert!(
                !state.can_transition_to(state),
                "{state:?} -> {state:?} should not be a move at all"
            );
        }
    }

    #[test]
    fn cleanup_eligibility_covers_every_state() {
        for state in ALL_STATES {
            let expected = matches!(
                state,
                ResourceState::Expired | ResourceState::PendingCleanup
            );
            assert_eq!(state.is_cleanup_eligible(), expected, "{state:?}");
        }
    }

    #[test]
    fn only_destroyed_is_terminal() {
        for state in ALL_STATES {
            assert_eq!(
                state.is_terminal(),
                state == ResourceState::Destroyed,
                "{state:?}"
            );
        }
    }

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
