mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, ExecutionClaimOutcome, ExecutionError, ExecutionState, IdempotencyKey,
    IdempotencyStore, InMemoryEvidenceStore, InMemoryIdempotencyStore, InMemoryJournal, Journal,
    JournalEntry, NoFaultInjector, RecoveryManager, RecoveryPlan,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Debug)]
struct FailAtJournal {
    fail_at: usize,
    calls: AtomicUsize,
    entries: Mutex<Vec<JournalEntry>>,
}

impl FailAtJournal {
    fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            calls: AtomicUsize::new(0),
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Journal for FailAtJournal {
    fn append(&self, entry: JournalEntry) -> Result<(), String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at {
            return Err(format!("journal failed on append {call}"));
        }
        self.entries.lock().unwrap().push(entry);
        Ok(())
    }

    fn entries(&self) -> Result<Vec<JournalEntry>, String> {
        Ok(self.entries.lock().unwrap().clone())
    }
}

#[derive(Debug)]
struct FailingEvidence;

impl EvidenceStore for FailingEvidence {
    fn persist(
        &self,
        _record: &execution_assurance_microkernel::ExecutionRecord,
    ) -> Result<(), String> {
        Err("evidence store unavailable".to_string())
    }

    fn find(
        &self,
        _execution_id: &str,
    ) -> Result<Option<execution_assurance_microkernel::ExecutionRecord>, String> {
        Ok(None)
    }
}

#[derive(Debug)]
struct FailingIdempotency;

impl IdempotencyStore for FailingIdempotency {
    fn claim_execution_id(&self, _execution_id: &str) -> Result<ExecutionClaimOutcome, String> {
        Err("idempotency store unavailable".to_string())
    }

    fn claim(
        &self,
        _key: &IdempotencyKey,
        _execution_id: &str,
    ) -> Result<execution_assurance_microkernel::idempotency::ClaimOutcome, String> {
        Err("idempotency store unavailable".to_string())
    }
}

#[test]
fn journal_failure_after_the_external_effect_never_becomes_success() {
    // Proposed, Validated, Prepared are the first three durable appends.
    // Failing the fourth simulates losing the local COMMITTED write after
    // the external effect has already occurred.
    let journal = Arc::new(FailAtJournal::new(4));
    let kernel = kernel_with(
        journal.clone(),
        Arc::new(InMemoryEvidenceStore::default()),
        Arc::new(InMemoryIdempotencyStore::default()),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    let error = kernel
        .execute(TestAction::default(), &mut world)
        .unwrap_err();

    assert!(matches!(error, ExecutionError::Journal(_)));
    assert_eq!(world.commits, 1);
    let directives = RecoveryManager::scan(journal.as_ref()).unwrap();
    assert_eq!(directives.len(), 1);
    assert_eq!(directives[0].plan, RecoveryPlan::ReconcileBeforeRetry);
    assert!(directives[0].recovery.is_some());
}

#[test]
fn evidence_failure_cannot_leave_a_durable_terminal_state() {
    let journal = Arc::new(InMemoryJournal::default());
    let kernel = kernel_with(
        journal.clone(),
        Arc::new(FailingEvidence),
        Arc::new(InMemoryIdempotencyStore::default()),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    let error = kernel
        .execute(TestAction::default(), &mut world)
        .unwrap_err();

    assert!(matches!(error, ExecutionError::EvidencePersistence(_)));
    assert_eq!(world.commits, 1);
    let entries = journal.entries().unwrap();
    assert_eq!(entries.last().unwrap().state, ExecutionState::Verified);
    assert!(!entries.last().unwrap().state.is_terminal());
}

#[test]
fn idempotency_store_failure_fails_before_the_effect() {
    let kernel = kernel_with(
        Arc::new(InMemoryJournal::default()),
        Arc::new(InMemoryEvidenceStore::default()),
        Arc::new(FailingIdempotency),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    let error = kernel
        .execute(TestAction::default(), &mut world)
        .unwrap_err();

    assert!(matches!(error, ExecutionError::Idempotency(_)));
    assert_eq!(world.commits, 0);
}

#[test]
fn invariant_violation_after_commit_rolls_back() {
    let kernel = kernel_with(
        Arc::new(InMemoryJournal::default()),
        Arc::new(InMemoryEvidenceStore::default()),
        Arc::new(InMemoryIdempotencyStore::default()),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();
    let action = TestAction {
        delta: -1,
        ..TestAction::default()
    };

    let result = kernel.execute(action, &mut world).unwrap();

    assert_eq!(
        result.outcome,
        execution_assurance_microkernel::ExecutionOutcome::RolledBack
    );
    assert_eq!(world.value, 0);
    assert_eq!(world.commits, 1);
    assert!(result
        .record
        .invariants_after
        .iter()
        .any(|check| !check.passed));
    assert!(result
        .record
        .invariants_after_rollback
        .iter()
        .all(|check| check.passed));
}
