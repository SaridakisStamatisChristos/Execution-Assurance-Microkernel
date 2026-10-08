mod common;

use common::{basic_kernel, CommitBehavior, TestAction, World};
use execution_assurance_microkernel::{CommitDisposition, ExecutionOutcome};

#[test]
fn unknown_commit_is_reconciled_before_success() {
    let mut world = World::default();
    let action = TestAction { commit_behavior: CommitBehavior::UnknownCommitted, ..TestAction::default() };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::Success);
    assert_eq!(world.commits, 1);
    assert_eq!(result.record.commit.disposition, CommitDisposition::ReconciledCommitted);
    assert!(result.record.reconciliation.unwrap().resolved);
}

#[test]
fn unresolved_unknown_commit_is_never_retried_blindly() {
    let mut world = World::default();
    let action = TestAction { commit_behavior: CommitBehavior::UnknownUnresolved, ..TestAction::default() };
    let result = basic_kernel().execute(action, &mut world).unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::ReconciliationRequired);
    assert_eq!(world.commits, 1);
    assert!(!result.record.reconciliation.unwrap().resolved);
}
