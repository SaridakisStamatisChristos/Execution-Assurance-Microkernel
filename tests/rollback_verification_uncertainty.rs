mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    ExecutionOutcome, ExecutionRequest, ExecutionState, FailureClass, InMemoryEvidenceStore,
    InMemoryIdempotencyStore, InMemoryJournal, Journal, NoFaultInjector,
};
use std::sync::Arc;

fn rollback_observer_unavailable() -> TestAction {
    TestAction {
        verify_ok: false,
        rollback_verify_error: true,
        ..TestAction::default()
    }
}

#[test]
fn rollback_verification_error_after_success_remains_recoverable() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(
        journal.clone(),
        evidence,
        ids,
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    let result = kernel
        .execute_request(
            ExecutionRequest::new(rollback_observer_unavailable())
                .with_execution_id("rollback-observer-error"),
            &mut world,
        )
        .unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::RollbackVerificationRequired);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 1);
    assert_eq!(world.value, 0);
    assert_eq!(
        journal.entries().unwrap().last().unwrap().state,
        ExecutionState::RollbackPending
    );
    assert!(result
        .record
        .rollback
        .as_ref()
        .unwrap()
        .verification_indeterminate);
    assert!(matches!(
        result.record.failure.as_ref().map(|failure| failure.class),
        Some(FailureClass::RollbackVerificationIndeterminate)
    ));
}

#[test]
fn repeated_rollback_observer_failure_never_repeats_compensation() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    let first = kernel
        .execute_request(
            ExecutionRequest::new(rollback_observer_unavailable())
                .with_execution_id("rollback-observer-retry"),
            &mut world,
        )
        .unwrap();
    let second = kernel
        .recover(
            "rollback-observer-retry",
            rollback_observer_unavailable(),
            &mut world,
        )
        .unwrap();
    let third = kernel
        .recover(
            "rollback-observer-retry",
            rollback_observer_unavailable(),
            &mut world,
        )
        .unwrap();

    assert_eq!(first.outcome, ExecutionOutcome::RollbackVerificationRequired);
    assert_eq!(second.outcome, ExecutionOutcome::RollbackVerificationRequired);
    assert_eq!(third.outcome, ExecutionOutcome::RollbackVerificationRequired);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 1);
    assert_eq!(world.value, 0);
}

#[test]
fn later_rollback_observation_finalizes_without_second_compensation() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    kernel
        .execute_request(
            ExecutionRequest::new(rollback_observer_unavailable())
                .with_execution_id("rollback-observer-recovers"),
            &mut world,
        )
        .unwrap();

    let recovered = kernel
        .recover(
            "rollback-observer-recovers",
            TestAction {
                verify_ok: false,
                ..TestAction::default()
            },
            &mut world,
        )
        .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::RolledBack);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 1);
    assert_eq!(world.value, 0);
    assert!(recovered.record.rollback.as_ref().unwrap().verified);
}

#[test]
fn explicit_negative_rollback_observation_allows_compensation_retry() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    kernel
        .execute_request(
            ExecutionRequest::new(rollback_observer_unavailable())
                .with_execution_id("rollback-observer-negative"),
            &mut world,
        )
        .unwrap();
    assert_eq!(world.rollbacks, 1);

    world.value = 7;
    let recovered = kernel
        .recover(
            "rollback-observer-negative",
            TestAction {
                verify_ok: false,
                ..TestAction::default()
            },
            &mut world,
        )
        .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::RolledBack);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 2);
    assert_eq!(world.value, 0);
}
