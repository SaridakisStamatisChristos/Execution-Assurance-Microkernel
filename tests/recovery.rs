mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    CompensationPolicy, EvidenceStore, ExecutionOutcome, ExecutionRequest, ExecutionState,
    FaultPoint, IdempotencyStore, InMemoryEvidenceStore, InMemoryIdempotencyStore, InMemoryJournal,
    Journal, JournalEntry, NoFaultInjector, RecoveryEnvelope, RecoveryManager, RecoveryPlan,
    ScriptedFaultInjector,
};
use std::sync::Arc;

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

#[test]
fn crash_after_effect_before_committed_recovers_without_second_commit() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let mut world = World::default();

    let crashing = kernel_with(
        journal.clone(),
        evidence.clone(),
        ids.clone(),
        Arc::new(ScriptedFaultInjector::with_fault(FaultPoint::AfterCommit)),
    );
    let error = crashing
        .execute_request(
            ExecutionRequest::new(TestAction::default()).with_execution_id("crash-after-effect"),
            &mut world,
        )
        .unwrap_err();
    assert!(error.to_string().contains("fault injected"));
    assert_eq!(world.commits, 1);

    let recovered = kernel_with(
        journal.clone(),
        evidence.clone(),
        ids,
        Arc::new(NoFaultInjector),
    )
    .recover("crash-after-effect", TestAction::default(), &mut world)
    .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::Success);
    assert_eq!(world.commits, 1);
    assert!(recovered.record.recovery.as_ref().unwrap().resumed);
    assert_eq!(
        journal.entries().unwrap().last().unwrap().state,
        ExecutionState::Finalized
    );
}

#[test]
fn crash_before_commit_recovers_to_aborted_without_effect() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let mut world = World::default();

    let crashing = kernel_with(
        journal.clone(),
        evidence.clone(),
        ids.clone(),
        Arc::new(ScriptedFaultInjector::with_fault(FaultPoint::BeforeCommit)),
    );
    crashing
        .execute_request(
            ExecutionRequest::new(TestAction::default()).with_execution_id("crash-before-commit"),
            &mut world,
        )
        .unwrap_err();
    assert_eq!(world.commits, 0);

    let recovered = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector))
        .recover("crash-before-commit", TestAction::default(), &mut world)
        .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::Aborted);
    assert_eq!(world.commits, 0);
}

#[test]
fn crash_after_rollback_effect_resumes_without_second_rollback() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let action = TestAction {
        verify_ok: false,
        ..TestAction::default()
    };
    let mut world = World::default();

    let crashing = kernel_with(
        journal.clone(),
        evidence.clone(),
        ids.clone(),
        Arc::new(ScriptedFaultInjector::with_fault(FaultPoint::AfterRollback)),
    );
    crashing
        .execute_request(
            ExecutionRequest::new(action.clone()).with_execution_id("crash-after-rollback"),
            &mut world,
        )
        .unwrap_err();
    assert_eq!(world.value, 0);
    assert_eq!(world.rollbacks, 1);

    let recovered = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector))
        .recover("crash-after-rollback", action, &mut world)
        .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::RolledBack);
    assert_eq!(world.value, 0);
    assert_eq!(world.rollbacks, 1);
    assert!(recovered.record.rollback.as_ref().unwrap().verified);
}

#[test]
fn durable_verified_marker_can_be_finalized_after_restart() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence: Arc<dyn EvidenceStore> = Arc::new(InMemoryEvidenceStore::default());
    let ids: Arc<dyn IdempotencyStore> = Arc::new(InMemoryIdempotencyStore::default());
    let envelope = RecoveryEnvelope {
        action_id: "test-action".to_string(),
        action_type: "test_action".to_string(),
        idempotency_key: None,
        started_at_ms: 0,
        compensation_policy: CompensationPolicy::Compensable,
        snapshot: serde_json::json!(0),
    };

    journal
        .append(JournalEntry::state(
            "verified-restart",
            ExecutionState::Proposed,
            1,
        ))
        .unwrap();
    journal
        .append(JournalEntry::state(
            "verified-restart",
            ExecutionState::Validated,
            2,
        ))
        .unwrap();
    journal
        .append(JournalEntry {
            execution_id: "verified-restart".to_string(),
            state: ExecutionState::Prepared,
            at_ms: 3,
            recovery: Some(envelope),
        })
        .unwrap();
    journal
        .append(JournalEntry::state(
            "verified-restart",
            ExecutionState::Committed,
            4,
        ))
        .unwrap();
    journal
        .append(JournalEntry::state(
            "verified-restart",
            ExecutionState::Verified,
            5,
        ))
        .unwrap();

    let kernel = kernel_with(journal.clone(), evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World {
        value: 1,
        commits: 1,
        rollbacks: 0,
    };
    let result = kernel
        .recover("verified-restart", TestAction::default(), &mut world)
        .unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::Success);
    assert_eq!(world.commits, 1);
    assert_eq!(
        journal.entries().unwrap().last().unwrap().state,
        ExecutionState::Finalized
    );
}
