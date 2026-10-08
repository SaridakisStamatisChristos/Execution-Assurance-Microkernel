mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, ExecutionOutcome, IdempotencyKey, IdempotencyStore, InMemoryEvidenceStore,
    InMemoryIdempotencyStore, InMemoryJournal, Journal, NoFaultInjector,
};
use std::sync::Arc;

#[test]
fn duplicate_key_never_commits_twice() {
    let journal: Arc<dyn Journal> = Arc::new(InMemoryJournal::default());
    let evidence: Arc<dyn EvidenceStore> = Arc::new(InMemoryEvidenceStore::default());
    let ids: Arc<dyn IdempotencyStore> = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(journal, evidence, ids, Arc::new(NoFaultInjector));
    let mut world = World::default();

    let first = kernel.execute_idempotent(TestAction::default(), &mut world, IdempotencyKey::from("same")).unwrap();
    let second = kernel.execute_idempotent(TestAction::default(), &mut world, IdempotencyKey::from("same")).unwrap();

    assert_eq!(first.outcome, ExecutionOutcome::Success);
    assert_eq!(second.outcome, ExecutionOutcome::Rejected);
    assert_eq!(world.commits, 1);
}
