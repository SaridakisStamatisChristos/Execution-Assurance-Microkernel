mod common;

use common::{basic_kernel, CommitBehavior, TestAction, World};
use execution_assurance_microkernel::{ExecutionOutcome, ExecutionState};

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
    assert!(!result.record.rollback.unwrap().succeeded);
}
