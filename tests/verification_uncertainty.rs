mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    ExecutionOutcome, ExecutionRequest, FailureClass, InMemoryEvidenceStore,
    InMemoryIdempotencyStore, InMemoryJournal, NoFaultInjector,
};
use std::sync::Arc;

#[test]
fn verification_observer_error_never_triggers_compensation() {
    let mut world = World::default();
    let action = TestAction {
        verify_error: true,
        ..TestAction::default()
    };

    let result = common::basic_kernel().execute(action, &mut world).unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::VerificationRequired);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 0);
    let verification = result.record.verification.as_ref().unwrap();
    assert!(!verification.passed);
    assert!(verification.indeterminate);
    assert!(matches!(
        result.record.failure.as_ref().map(|failure| failure.class),
        Some(FailureClass::VerificationIndeterminate)
    ));
}

#[test]
fn verification_can_recover_later_without_a_second_commit() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    let first = kernel
        .execute_request(
            ExecutionRequest::new(TestAction {
                verify_error: true,
                ..TestAction::default()
            })
            .with_execution_id("verification-later"),
            &mut world,
        )
        .unwrap();
    assert_eq!(first.outcome, ExecutionOutcome::VerificationRequired);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 0);

    let recovered = kernel
        .recover("verification-later", TestAction::default(), &mut world)
        .unwrap();

    assert_eq!(recovered.outcome, ExecutionOutcome::Success);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 0);
    assert!(recovered.record.verification.as_ref().unwrap().passed);
}

#[test]
fn repeated_observer_failure_remains_recoverable_without_effects() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();
    let action = TestAction {
        verify_error: true,
        ..TestAction::default()
    };

    let first = kernel
        .execute_request(
            ExecutionRequest::new(action.clone()).with_execution_id("verification-retry"),
            &mut world,
        )
        .unwrap();
    let second = kernel
        .recover("verification-retry", action, &mut world)
        .unwrap();

    assert_eq!(first.outcome, ExecutionOutcome::VerificationRequired);
    assert_eq!(second.outcome, ExecutionOutcome::VerificationRequired);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 0);
}

#[test]
fn later_explicit_verification_failure_can_compensate() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    kernel
        .execute_request(
            ExecutionRequest::new(TestAction {
                verify_error: true,
                ..TestAction::default()
            })
            .with_execution_id("verification-fails-later"),
            &mut world,
        )
        .unwrap();

    let result = kernel
        .recover(
            "verification-fails-later",
            TestAction {
                verify_ok: false,
                ..TestAction::default()
            },
            &mut world,
        )
        .unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::RolledBack);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 1);
    assert_eq!(world.value, 0);
}
