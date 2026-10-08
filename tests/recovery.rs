use execution_assurance_microkernel::{
    ExecutionState, InMemoryJournal, Journal, JournalEntry, RecoveryManager, RecoveryPlan,
};

#[test]
fn prepared_execution_requires_reconciliation_before_retry() {
    let journal = InMemoryJournal::default();
    journal
        .append(JournalEntry::state("x", ExecutionState::Prepared, 1))
        .unwrap();
    let directives = RecoveryManager::scan(&journal).unwrap();
    assert_eq!(directives.len(), 1);
    assert_eq!(directives[0].plan, RecoveryPlan::ReconcileBeforeRetry);
}

#[test]
fn committed_execution_requires_verification_or_rollback() {
    assert_eq!(
        RecoveryManager::classify(ExecutionState::Committed),
        RecoveryPlan::VerifyOrRollback
    );
    assert_eq!(
        RecoveryManager::classify(ExecutionState::RollbackPending),
        RecoveryPlan::CompleteRollback
    );
    assert_eq!(
        RecoveryManager::classify(ExecutionState::Verified),
        RecoveryPlan::FinalizeVerified
    );
    assert_eq!(
        RecoveryManager::classify(ExecutionState::ReconciliationRequired),
        RecoveryPlan::ReconcileBeforeRetry
    );
    assert_eq!(
        RecoveryManager::classify(ExecutionState::Finalized),
        RecoveryPlan::None
    );
}
