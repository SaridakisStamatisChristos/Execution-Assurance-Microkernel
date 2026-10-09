mod common;

use common::{basic_kernel, CommitBehavior, TestAction, World};
use execution_assurance_microkernel::{ExecutionOutcome, ExecutionState, FailureClass};

#[test]
fn success_requires_verified_postcondition() {
    let mut world = World::default();
    let result = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::Success);
    assert_eq!(result.output, Some(1));
    assert_eq!(world.commits, 1);
    assert_eq!(
        result.record.transitions.last().unwrap().to,
        ExecutionState::Finalized
    );
    assert!(result.record.verification.as_ref().unwrap().passed);
    assert!(result.record.record_hash.is_some());
}

#[test]
fn failed_precondition_rejects_without_commit() {
    let mut world = World::default();
    let action = TestAction {
        allowed: false,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::Rejected);
    assert_eq!(world.commits, 0);
    assert!(result
        .record
        .preconditions
        .iter()
        .any(|check| !check.passed));
}

#[test]
fn known_commit_failure_is_not_success() {
    let mut world = World::default();
    let action = TestAction {
        commit_behavior: CommitBehavior::Failed,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::CommitFailed);
    assert_eq!(world.commits, 0);
}

#[test]
fn verification_failure_triggers_verified_rollback() {
    let mut world = World::default();
    let action = TestAction {
        verify_ok: false,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::RolledBack);
    assert_eq!(world.value, 0);
    assert_eq!(world.rollbacks, 1);
    let rollback = result.record.rollback.unwrap();
    assert!(rollback.succeeded);
    assert!(rollback.verified);
}

#[test]
fn rollback_failure_is_never_hidden() {
    let mut world = World::default();
    let action = TestAction {
        verify_ok: false,
        rollback_ok: false,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::RollbackFailed);
    assert_eq!(world.value, 1);
    assert_eq!(world.rollbacks, 1);
    assert!(!result.record.rollback.unwrap().succeeded);
}

#[test]
fn partial_rollback_failure_is_explicit_and_preserves_failed_state() {
    let mut world = World::default();
    let action = TestAction {
        verify_ok: false,
        partial_rollback: true,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::RollbackFailed);
    assert_eq!(world.rollbacks, 1);
    assert_ne!(world.value, 0);
    let rollback = result.record.rollback.unwrap();
    assert!(rollback.attempted);
    assert!(!rollback.succeeded);
    assert!(!rollback.verified);
}

#[test]
fn compensation_conflict_is_explicit_and_preserves_external_state() {
    let mut world = World::default();
    let action = TestAction {
        verify_ok: false,
        rollback_conflict: true,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::RollbackFailed);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 1);
    assert_eq!(world.value, 1);
    assert!(matches!(
        result.record.failure.as_ref().map(|failure| failure.class),
        Some(FailureClass::CompensationConflict)
    ));
    let rollback = result.record.rollback.unwrap();
    assert!(rollback.attempted);
    assert!(!rollback.succeeded);
    assert!(!rollback.verified);
}

#[test]
fn non_compensable_action_never_claims_rollback_success() {
    let mut world = World::default();
    let action = TestAction {
        verify_ok: false,
        compensable: false,
        ..TestAction::default()
    };
    let result = basic_kernel().execute(action, &mut world).unwrap();

    assert_eq!(result.outcome, ExecutionOutcome::RollbackFailed);
    assert_eq!(world.commits, 1);
    assert_eq!(world.rollbacks, 0);
    assert!(matches!(
        result.record.failure.as_ref().map(|failure| failure.class),
        Some(FailureClass::CompensationUnavailable)
    ));
    let rollback = result.record.rollback.unwrap();
    assert!(!rollback.attempted);
    assert!(!rollback.verified);
}
