mod common;

use common::{basic_kernel, kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, ExecutionError, ExecutionOutcome, ExecutionRecord, ExecutionRequest,
    InMemoryEvidenceStore, InMemoryIdempotencyStore, InMemoryJournal, NoFaultInjector,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};

#[derive(Debug, Default)]
struct BlockingEvidenceStore {
    inner: InMemoryEvidenceStore,
    block_finds: AtomicBool,
    entered: (Mutex<bool>, Condvar),
    release: (Mutex<bool>, Condvar),
}

impl BlockingEvidenceStore {
    fn enable_blocking_find(&self) {
        self.block_finds.store(true, Ordering::SeqCst);
    }

    fn wait_until_find_is_blocked(&self) {
        let (lock, condvar) = &self.entered;
        let mut entered = lock.lock().unwrap();
        while !*entered {
            entered = condvar.wait(entered).unwrap();
        }
    }

    fn release_find(&self) {
        let (lock, condvar) = &self.release;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
    }
}

impl EvidenceStore for BlockingEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        self.inner.persist(record)
    }

    fn find(&self, execution_id: &str) -> Result<Option<ExecutionRecord>, String> {
        if self.block_finds.load(Ordering::SeqCst) {
            let (entered_lock, entered_condvar) = &self.entered;
            *entered_lock
                .lock()
                .map_err(|_| "entered lock poisoned".to_string())? = true;
            entered_condvar.notify_all();

            let (release_lock, release_condvar) = &self.release;
            let mut released = release_lock
                .lock()
                .map_err(|_| "release lock poisoned".to_string())?;
            while !*released {
                released = release_condvar
                    .wait(released)
                    .map_err(|_| "release lock poisoned".to_string())?;
            }
        }
        self.inner.find(execution_id)
    }
}

#[derive(Debug)]
struct StaticEvidenceStore {
    record: Mutex<ExecutionRecord>,
}

impl StaticEvidenceStore {
    fn new(record: ExecutionRecord) -> Self {
        Self {
            record: Mutex::new(record),
        }
    }
}

impl EvidenceStore for StaticEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        *self
            .record
            .lock()
            .map_err(|_| "static evidence lock poisoned".to_string())? = record.clone();
        Ok(())
    }

    fn find(&self, execution_id: &str) -> Result<Option<ExecutionRecord>, String> {
        let record = self
            .record
            .lock()
            .map_err(|_| "static evidence lock poisoned".to_string())?;
        Ok((record.execution_id == execution_id).then(|| record.clone()))
    }
}

#[test]
fn concurrent_recovery_for_same_execution_fails_closed_in_process() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(BlockingEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(
        journal,
        evidence.clone(),
        ids,
        Arc::new(NoFaultInjector),
    );
    let action = TestAction {
        verify_error: true,
        ..TestAction::default()
    };
    let mut world = World::default();

    let initial = kernel
        .execute_request(
            ExecutionRequest::new(action.clone()).with_execution_id("serialized-recovery"),
            &mut world,
        )
        .unwrap();
    assert_eq!(initial.outcome, ExecutionOutcome::VerificationRequired);

    evidence.enable_blocking_find();
    let first_kernel = kernel.clone();
    let first_action = action.clone();
    let first = thread::spawn(move || {
        let mut first_world = World {
            value: 1,
            commits: 1,
            rollbacks: 0,
        };
        let result = first_kernel.recover(
            "serialized-recovery",
            first_action,
            &mut first_world,
        );
        (result, first_world)
    });

    evidence.wait_until_find_is_blocked();

    let mut contender_world = World {
        value: 1,
        commits: 1,
        rollbacks: 0,
    };
    let error = kernel
        .recover("serialized-recovery", action, &mut contender_world)
        .unwrap_err();
    assert!(matches!(
        error,
        ExecutionError::RecoveryInProgress(ref id) if id == "serialized-recovery"
    ));
    assert_eq!(contender_world.value, 1);
    assert_eq!(contender_world.commits, 1);
    assert_eq!(contender_world.rollbacks, 0);

    evidence.release_find();
    let (first_result, first_world) = first.join().unwrap();
    let first_result = first_result.unwrap();
    assert_eq!(first_result.outcome, ExecutionOutcome::VerificationRequired);
    assert_eq!(first_world.commits, 1);
    assert_eq!(first_world.rollbacks, 0);
}

#[test]
fn recovery_rejects_tampered_terminal_evidence_before_trusting_it() {
    let journal = Arc::new(InMemoryJournal::default());
    let evidence = Arc::new(InMemoryEvidenceStore::default());
    let ids = Arc::new(InMemoryIdempotencyStore::default());
    let kernel = kernel_with(
        journal.clone(),
        evidence,
        ids.clone(),
        Arc::new(NoFaultInjector),
    );
    let mut world = World::default();

    let result = kernel
        .execute_request(
            ExecutionRequest::new(TestAction::default()).with_execution_id("tampered-evidence"),
            &mut world,
        )
        .unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::Success);

    let mut tampered = result.record;
    tampered.action_id.push_str("-tampered");
    assert!(!tampered.verify_hash().unwrap());

    let recovery_kernel = kernel_with(
        journal,
        Arc::new(StaticEvidenceStore::new(tampered)),
        ids,
        Arc::new(NoFaultInjector),
    );
    let before = (world.value, world.commits, world.rollbacks);
    let error = recovery_kernel
        .recover("tampered-evidence", TestAction::default(), &mut world)
        .unwrap_err();

    assert!(matches!(error, ExecutionError::EvidenceIntegrity(_)));
    assert_eq!((world.value, world.commits, world.rollbacks), before);
}

#[test]
fn schema3_seal_remains_verifiable_after_legacy_field_omission() {
    let mut world = World::default();
    let mut record = basic_kernel()
        .execute(
            TestAction {
                verify_ok: false,
                ..TestAction::default()
            },
            &mut world,
        )
        .unwrap()
        .record;

    record.schema_version = 3;
    record.record_hash = None;
    let sealed = record.seal().unwrap();
    let mut value = serde_json::to_value(sealed).unwrap();
    value["rollback"]
        .as_object_mut()
        .unwrap()
        .remove("verification_indeterminate");

    let decoded: ExecutionRecord = serde_json::from_value(value).unwrap();
    assert!(decoded.verify_hash().unwrap());
}

#[test]
fn schema2_seal_remains_verifiable_after_legacy_field_omission() {
    let mut world = World::default();
    let mut record = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap()
        .record;

    record.schema_version = 2;
    record.record_hash = None;
    let sealed = record.seal().unwrap();
    let mut value = serde_json::to_value(sealed).unwrap();
    value["verification"]
        .as_object_mut()
        .unwrap()
        .remove("indeterminate");

    let decoded: ExecutionRecord = serde_json::from_value(value).unwrap();
    assert!(decoded.verify_hash().unwrap());
}
