mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, ExecutionClaimOutcome, ExecutionError, ExecutionOutcome, ExecutionRequest,
    FileIdempotencyStore, IdempotencyKey, IdempotencyStore, InMemoryEvidenceStore,
    InMemoryIdempotencyStore, InMemoryJournal, Journal, NoFaultInjector,
};
use std::{fs, sync::Arc};
use uuid::Uuid;

#[test]
fn duplicate_key_never_commits_twice() {
    let journal: Arc<dyn Journal> = Arc::new(InMemoryJournal::default());
    let evidence: Arc<dyn EvidenceStore> = Arc::new(InMemoryEvidenceStore::default());
    let ids: Arc<dyn IdempotencyStore> = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    let first = kernel
        .execute_idempotent(
            TestAction::default(),
            &mut world,
            IdempotencyKey::from("same"),
        )
        .unwrap();
    let second = kernel
        .execute_idempotent(
            TestAction::default(),
            &mut world,
            IdempotencyKey::from("same"),
        )
        .unwrap();

    assert_eq!(first.outcome, ExecutionOutcome::Success);
    assert_eq!(second.outcome, ExecutionOutcome::Rejected);
    assert_eq!(world.commits, 1);
}

#[test]
fn duplicate_execution_id_fails_closed_before_second_effect() {
    let kernel = kernel_with(
        Arc::new(InMemoryJournal::default()),
        Arc::new(InMemoryEvidenceStore::default()),
        Arc::new(InMemoryIdempotencyStore::default()),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    kernel
        .execute_request(
            ExecutionRequest::new(TestAction::default()).with_execution_id("exec-fixed"),
            &mut world,
        )
        .unwrap();
    let error = kernel
        .execute_request(
            ExecutionRequest::new(TestAction::default()).with_execution_id("exec-fixed"),
            &mut world,
        )
        .unwrap_err();

    assert!(matches!(error, ExecutionError::DuplicateExecutionId(value) if value == "exec-fixed"));
    assert_eq!(world.commits, 1);
}

#[test]
fn durable_claims_survive_store_reopen() {
    let root = std::env::temp_dir().join(format!("eamk-claims-{}", Uuid::new_v4()));
    let key = IdempotencyKey::from("durable-key");

    {
        let store = FileIdempotencyStore::open(&root).unwrap();
        assert_eq!(
            store.claim_execution_id("exec-1").unwrap(),
            ExecutionClaimOutcome::Claimed
        );
        assert!(matches!(
            store.claim(&key, "exec-1").unwrap(),
            execution_assurance_microkernel::idempotency::ClaimOutcome::Claimed
        ));
    }

    let reopened = FileIdempotencyStore::open(&root).unwrap();
    assert_eq!(
        reopened.claim_execution_id("exec-1").unwrap(),
        ExecutionClaimOutcome::Duplicate
    );
    assert!(matches!(
        reopened.claim(&key, "exec-2").unwrap(),
        execution_assurance_microkernel::idempotency::ClaimOutcome::Duplicate {
            original_execution_id
        } if original_execution_id == "exec-1"
    ));

    fs::remove_dir_all(root).unwrap();
}
