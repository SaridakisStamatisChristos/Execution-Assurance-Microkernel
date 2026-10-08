use execution_assurance_microkernel::ExecutionState;

#[test]
fn invalid_transitions_are_rejected_by_relation() {
    assert!(!ExecutionState::Proposed.can_transition_to(ExecutionState::Committed));
    assert!(!ExecutionState::Prepared.can_transition_to(ExecutionState::Verified));
    assert!(!ExecutionState::Committed.can_transition_to(ExecutionState::Finalized));
    assert!(ExecutionState::Committed.can_transition_to(ExecutionState::RollbackPending));
    assert!(ExecutionState::Verified.can_transition_to(ExecutionState::Finalized));
}
