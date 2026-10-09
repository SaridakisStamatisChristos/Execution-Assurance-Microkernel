use crate::{
    action::{
        Action, CommitStatus, CompensationPolicy, EffectPermit, ReconciliationResult,
        RollbackStatus,
    },
    error::{ExecutionError, FailureClass},
    evidence::{
        CheckRecord, CommitDisposition, CommitRecord, ConservativeRedactor, EvidenceRedactor,
        EvidenceStore, ExecutionOutcome, ExecutionRecord, ExecutionResult, FailureRecord,
        InMemoryEvidenceStore, ReconciliationRecord, RecoveryRecord, RollbackRecord,
        VerificationRecord,
    },
    fault::{FaultInjector, FaultPoint, NoFaultInjector},
    idempotency::{
        ClaimOutcome, ExecutionClaimOutcome, IdempotencyKey, IdempotencyStore,
        InMemoryIdempotencyStore,
    },
    invariant::InvariantPhase,
    journal::{InMemoryJournal, Journal, JournalEntry, RecoveryEnvelope},
    recovery::{RecoveryManager, RecoveryPlan},
    state::{ExecutionState, ExecutionTrace},
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

#[derive(Debug, Default)]
pub struct UuidIdGenerator;

impl IdGenerator for UuidIdGenerator {
    fn next_id(&self) -> String {
        Uuid::new_v4().to_string()
    }
}

#[derive(Debug, Default)]
pub struct SequenceIdGenerator {
    next: AtomicU64,
}

impl SequenceIdGenerator {
    pub fn starting_at(value: u64) -> Self {
        Self {
            next: AtomicU64::new(value),
        }
    }
}

impl IdGenerator for SequenceIdGenerator {
    fn next_id(&self) -> String {
        format!("exec-{}", self.next.fetch_add(1, Ordering::SeqCst))
    }
}

static ACTIVE_RECOVERIES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[derive(Debug)]
struct LocalRecoveryGuard {
    execution_id: String,
}

impl LocalRecoveryGuard {
    fn acquire(execution_id: &str) -> Result<Self, ExecutionError> {
        let active = ACTIVE_RECOVERIES.get_or_init(|| Mutex::new(HashSet::new()));
        let mut active = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.insert(execution_id.to_string()) {
            return Err(ExecutionError::RecoveryInProgress(execution_id.to_string()));
        }
        Ok(Self {
            execution_id: execution_id.to_string(),
        })
    }
}

impl Drop for LocalRecoveryGuard {
    fn drop(&mut self) {
        if let Some(active) = ACTIVE_RECOVERIES.get() {
            let mut active = active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            active.remove(&self.execution_id);
        }
    }
}

#[derive(Debug)]
pub struct ExecutionRequest<A> {
    pub execution_id: Option<String>,
    pub idempotency_key: Option<IdempotencyKey>,
    pub action: A,
}

impl<A> ExecutionRequest<A> {
    pub fn new(action: A) -> Self {
        Self {
            execution_id: None,
            idempotency_key: None,
            action,
        }
    }

    pub fn with_execution_id(mut self, execution_id: impl Into<String>) -> Self {
        self.execution_id = Some(execution_id.into());
        self
    }

    pub fn with_idempotency_key(mut self, key: impl Into<IdempotencyKey>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }
}

#[derive(Clone)]
pub struct Kernel {
    journal: Arc<dyn Journal>,
    evidence: Arc<dyn EvidenceStore>,
    idempotency: Arc<dyn IdempotencyStore>,
    faults: Arc<dyn FaultInjector>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    redactor: Arc<dyn EvidenceRedactor>,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Kernel").finish_non_exhaustive()
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self {
            journal: Arc::new(InMemoryJournal::default()),
            evidence: Arc::new(InMemoryEvidenceStore::default()),
            idempotency: Arc::new(InMemoryIdempotencyStore::default()),
            faults: Arc::new(NoFaultInjector),
            clock: Arc::new(SystemClock),
            ids: Arc::new(UuidIdGenerator),
            redactor: Arc::new(ConservativeRedactor::default()),
        }
    }
}

impl Kernel {
    pub fn with_components(
        journal: Arc<dyn Journal>,
        evidence: Arc<dyn EvidenceStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        faults: Arc<dyn FaultInjector>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self::with_components_and_redactor(
            journal,
            evidence,
            idempotency,
            faults,
            clock,
            ids,
            Arc::new(ConservativeRedactor::default()),
        )
    }

    pub fn with_components_and_redactor(
        journal: Arc<dyn Journal>,
        evidence: Arc<dyn EvidenceStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        faults: Arc<dyn FaultInjector>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        redactor: Arc<dyn EvidenceRedactor>,
    ) -> Self {
        Self {
            journal,
            evidence,
            idempotency,
            faults,
            clock,
            ids,
            redactor,
        }
    }

    pub fn execute<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        self.execute_request(ExecutionRequest::new(action), ctx)
    }

    pub fn execute_idempotent<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
        key: IdempotencyKey,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        self.execute_request(ExecutionRequest::new(action).with_idempotency_key(key), ctx)
    }

    pub fn execute_request<A: Action>(
        &self,
        request: ExecutionRequest<A>,
        ctx: &mut A::Context,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let execution_id = request.execution_id.unwrap_or_else(|| self.ids.next_id());
        match self
            .idempotency
            .claim_execution_id(&execution_id)
            .map_err(ExecutionError::Idempotency)?
        {
            ExecutionClaimOutcome::Claimed => {}
            ExecutionClaimOutcome::Duplicate => {
                return Err(ExecutionError::DuplicateExecutionId(execution_id));
            }
        }
        self.execute_claimed(request.action, ctx, execution_id, request.idempotency_key)
    }

    pub fn recover<A: Action>(
        &self,
        execution_id: &str,
        action: A,
        ctx: &mut A::Context,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        // Recovery ownership is process-local: concurrent recoveries for the
        // same execution fail closed before reading evidence or invoking action
        // hooks. Cross-process/distributed fencing remains an external concern.
        let _recovery_guard = LocalRecoveryGuard::acquire(execution_id)?;

        if let Some(existing) = self
            .evidence
            .find(execution_id)
            .map_err(ExecutionError::EvidencePersistence)?
        {
            match existing.verify_hash() {
                Ok(true) => {}
                Ok(false) => {
                    return Err(ExecutionError::EvidenceIntegrity(format!(
                        "execution {execution_id} has missing or invalid evidence seal"
                    )));
                }
                Err(error) => {
                    return Err(ExecutionError::EvidenceIntegrity(format!(
                        "execution {execution_id} evidence seal could not be verified: {error}"
                    )));
                }
            }
            if existing
                .transitions
                .last()
                .is_some_and(|transition| transition.to.is_terminal())
            {
                return Err(ExecutionError::AlreadyFinalized(execution_id.to_string()));
            }
        }

        let directive = RecoveryManager::directive(self.journal.as_ref(), execution_id)
            .map_err(ExecutionError::Journal)?
            .ok_or_else(|| ExecutionError::RecoveryUnavailable(execution_id.to_string()))?;
        let envelope = directive.recovery.clone();
        if let Some(envelope) = envelope.as_ref() {
            if envelope.action_id != action.action_id()
                || envelope.action_type != action.action_type()
            {
                return Err(ExecutionError::RecoveryMismatch(format!(
                    "journal has {}/{} but supplied action is {}/{}",
                    envelope.action_type,
                    envelope.action_id,
                    action.action_type(),
                    action.action_id()
                )));
            }
        }

        let mut trace = self.rebuild_trace(execution_id)?;
        let started_at_ms = envelope
            .as_ref()
            .map_or_else(|| self.clock.now_ms(), |value| value.started_at_ms);
        let idempotency_key = envelope
            .as_ref()
            .and_then(|value| value.idempotency_key.clone());
        let mut record = self.new_record(
            execution_id.to_string(),
            &action,
            idempotency_key,
            started_at_ms,
        );
        record.recovery = Some(RecoveryRecord {
            resumed: true,
            from_state: directive.last_state,
            detail: format!("recovery plan: {:?}", directive.plan),
        });

        match directive.plan {
            RecoveryPlan::None => Err(ExecutionError::RecoveryUnavailable(
                execution_id.to_string(),
            )),
            RecoveryPlan::AbortBeforeCommit => {
                record.failure = Some(FailureRecord {
                    class: FailureClass::RecoveryFailed,
                    message: "interrupted before durable preparation; no effect may be retried implicitly"
                        .to_string(),
                });
                let terminal = if trace.state() == ExecutionState::Proposed {
                    ExecutionState::Rejected
                } else {
                    ExecutionState::Aborted
                };
                self.finish_terminal(
                    record,
                    &mut trace,
                    terminal,
                    ExecutionOutcome::Aborted,
                    None,
                )
            }
            RecoveryPlan::ReconcileBeforeRetry => {
                let envelope = envelope.ok_or_else(|| {
                    ExecutionError::RecoveryData("prepared recovery envelope missing".to_string())
                })?;
                let snapshot = self.decode_snapshot::<A>(&envelope)?;
                if trace.state() == ExecutionState::Prepared {
                    self.transition(
                        execution_id,
                        &mut trace,
                        ExecutionState::ReconciliationRequired,
                    )?;
                }
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Unknown,
                    detail: "commit status reconstructed as uncertain".to_string(),
                };
                self.reconcile_recovery(&action, ctx, snapshot, record, trace)
            }
            RecoveryPlan::VerifyOrRollback => {
                let envelope = envelope.ok_or_else(|| {
                    ExecutionError::RecoveryData("prepared recovery envelope missing".to_string())
                })?;
                let snapshot = self.decode_snapshot::<A>(&envelope)?;
                self.transition(
                    execution_id,
                    &mut trace,
                    ExecutionState::ReconciliationRequired,
                )?;
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Confirmed,
                    detail: "durable committed marker recovered".to_string(),
                };
                self.reconcile_recovery(&action, ctx, snapshot, record, trace)
            }
            RecoveryPlan::CompleteRollback => {
                let envelope = envelope.ok_or_else(|| {
                    ExecutionError::RecoveryData("rollback snapshot missing".to_string())
                })?;
                let snapshot = self.decode_snapshot::<A>(&envelope)?;
                self.resume_rollback(&action, ctx, &snapshot, record, &mut trace)
            }
            RecoveryPlan::FinalizeVerified => {
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Confirmed,
                    detail: "durable verified marker implies established commit".to_string(),
                };
                record.verification = Some(VerificationRecord {
                    passed: true,
                    indeterminate: false,
                    checks: vec![CheckRecord::pass(
                        "durable_verified_marker",
                        "postconditions were durably marked verified before interruption",
                    )],
                    detail: "verification recovered from journal".to_string(),
                });
                self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Finalized,
                    ExecutionOutcome::Success,
                    None,
                )
            }
        }
    }

    fn execute_claimed<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
        execution_id: String,
        key: Option<IdempotencyKey>,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let started = self.clock.now_ms();
        let mut trace = ExecutionTrace::new();
        let mut record = self.new_record(
            execution_id.clone(),
            &action,
            key.as_ref().map(|value| value.0.clone()),
            started,
        );

        self.transition(&execution_id, &mut trace, ExecutionState::Proposed)?;

        if let Some(ref key) = key {
            match self
                .idempotency
                .claim(key, &execution_id)
                .map_err(ExecutionError::Idempotency)?
            {
                ClaimOutcome::Claimed => {}
                ClaimOutcome::Duplicate {
                    original_execution_id,
                } => {
                    record.failure = Some(FailureRecord {
                        class: FailureClass::IdempotencyConflict,
                        message: format!(
                            "idempotency key already claimed by {original_execution_id}"
                        ),
                    });
                    return self.finish_terminal(
                        record,
                        &mut trace,
                        ExecutionState::Rejected,
                        ExecutionOutcome::Rejected,
                        None,
                    );
                }
            }
        }

        match action.validate(ctx) {
            Ok(()) => record
                .validation
                .push(CheckRecord::pass("action_validation", "validation passed")),
            Err(error) => {
                record
                    .validation
                    .push(CheckRecord::fail("action_validation", error.to_string()));
                record.failure = Some(FailureRecord {
                    class: FailureClass::ValidationFailed,
                    message: error.to_string(),
                });
                return self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Rejected,
                    ExecutionOutcome::Rejected,
                    None,
                );
            }
        }

        record.preconditions = action
            .preconditions()
            .into_iter()
            .map(|predicate| predicate.evaluate(ctx))
            .collect();
        if let Some(failed) = record.preconditions.iter().find(|check| !check.passed) {
            record.failure = Some(FailureRecord {
                class: FailureClass::PreconditionFailed,
                message: format!("{}: {}", failed.name, failed.reason),
            });
            return self.finish_terminal(
                record,
                &mut trace,
                ExecutionState::Rejected,
                ExecutionOutcome::Rejected,
                None,
            );
        }

        self.transition(&execution_id, &mut trace, ExecutionState::Validated)?;
        self.fault(FaultPoint::BeforeSnapshot)?;
        let snapshot = match action.snapshot(ctx) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                record.failure = Some(FailureRecord {
                    class: FailureClass::SnapshotFailed,
                    message: error.to_string(),
                });
                return self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Aborted,
                    ExecutionOutcome::Aborted,
                    None,
                );
            }
        };
        self.fault(FaultPoint::AfterSnapshot)?;

        record.invariants_before = action
            .invariants()
            .into_iter()
            .map(|invariant| invariant.check(InvariantPhase::BeforeCommit, ctx))
            .collect();
        if let Some(failed) = record.invariants_before.iter().find(|check| !check.passed) {
            record.failure = Some(FailureRecord {
                class: FailureClass::InvariantViolation,
                message: format!("{}: {}", failed.name, failed.reason),
            });
            return self.finish_terminal(
                record,
                &mut trace,
                ExecutionState::Aborted,
                ExecutionOutcome::Aborted,
                None,
            );
        }

        let snapshot_json = match serde_json::to_value(&snapshot) {
            Ok(value) => value,
            Err(error) => {
                record.failure = Some(FailureRecord {
                    class: FailureClass::SnapshotFailed,
                    message: format!("snapshot serialization failed: {error}"),
                });
                return self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Aborted,
                    ExecutionOutcome::Aborted,
                    None,
                );
            }
        };
        let recovery = RecoveryEnvelope {
            action_id: action.action_id(),
            action_type: action.action_type().to_string(),
            idempotency_key: key.as_ref().map(|value| value.0.clone()),
            started_at_ms: started,
            compensation_policy: action.compensation_policy(),
            snapshot: snapshot_json,
        };
        self.transition_with_recovery(
            &execution_id,
            &mut trace,
            ExecutionState::Prepared,
            recovery,
        )?;
        self.fault(FaultPoint::BeforeCommit)?;
        self.fault(FaultPoint::DuringCommit)?;

        debug_assert!(record.validation.iter().all(|check| check.passed));
        debug_assert!(record.preconditions.iter().all(|check| check.passed));
        debug_assert!(record.invariants_before.iter().all(|check| check.passed));
        debug_assert_eq!(trace.state(), ExecutionState::Prepared);

        let permit = EffectPermit::new();
        let output = match action.commit(&permit, ctx) {
            CommitStatus::Confirmed(output) => {
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Confirmed,
                    detail: "commit confirmed".to_string(),
                };
                self.fault(FaultPoint::AfterCommit)?;
                self.transition(&execution_id, &mut trace, ExecutionState::Committed)?;
                output
            }
            CommitStatus::Failed(error) => {
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Failed,
                    detail: error.to_string(),
                };
                record.failure = Some(FailureRecord {
                    class: FailureClass::CommitFailed,
                    message: error.to_string(),
                });
                return self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::CommitFailed,
                    None,
                );
            }
            CommitStatus::Unknown { reason } => {
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Unknown,
                    detail: reason.clone(),
                };
                record.failure = Some(FailureRecord {
                    class: FailureClass::CommitOutcomeUnknown,
                    message: reason,
                });
                self.transition(
                    &execution_id,
                    &mut trace,
                    ExecutionState::ReconciliationRequired,
                )?;
                match action.reconcile(&permit, ctx) {
                    Ok(ReconciliationResult::Committed(output)) => {
                        record.reconciliation = Some(ReconciliationRecord {
                            attempted: true,
                            resolved: true,
                            detail: "reconciliation confirmed commit".to_string(),
                        });
                        record.commit = CommitRecord {
                            disposition: CommitDisposition::ReconciledCommitted,
                            detail: "commit established by reconciliation".to_string(),
                        };
                        record.failure = None;
                        self.transition(&execution_id, &mut trace, ExecutionState::Committed)?;
                        output
                    }
                    Ok(ReconciliationResult::NotCommitted) => {
                        record.reconciliation = Some(ReconciliationRecord {
                            attempted: true,
                            resolved: true,
                            detail: "reconciliation established no commit".to_string(),
                        });
                        record.commit = CommitRecord {
                            disposition: CommitDisposition::ReconciledNotCommitted,
                            detail: "no external effect observed".to_string(),
                        };
                        return self.finish_terminal(
                            record,
                            &mut trace,
                            ExecutionState::Aborted,
                            ExecutionOutcome::Aborted,
                            None,
                        );
                    }
                    Ok(ReconciliationResult::Unresolved { reason }) => {
                        record.reconciliation = Some(ReconciliationRecord {
                            attempted: true,
                            resolved: false,
                            detail: reason.clone(),
                        });
                        record.failure = Some(FailureRecord {
                            class: FailureClass::ReconciliationFailed,
                            message: reason,
                        });
                        return self.persist_checkpoint(
                            record,
                            &trace,
                            ExecutionOutcome::ReconciliationRequired,
                            None,
                        );
                    }
                    Err(error) => {
                        record.reconciliation = Some(ReconciliationRecord {
                            attempted: true,
                            resolved: false,
                            detail: error.to_string(),
                        });
                        record.failure = Some(FailureRecord {
                            class: FailureClass::ReconciliationFailed,
                            message: error.to_string(),
                        });
                        return self.persist_checkpoint(
                            record,
                            &trace,
                            ExecutionOutcome::ReconciliationRequired,
                            None,
                        );
                    }
                }
            }
        };

        self.after_commit(&action, ctx, &snapshot, output, record, &mut trace)
    }

    fn reconcile_recovery<A: Action>(
        &self,
        action: &A,
        ctx: &mut A::Context,
        snapshot: A::Snapshot,
        mut record: ExecutionRecord,
        mut trace: ExecutionTrace,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let permit = EffectPermit::new();
        match action.reconcile(&permit, ctx) {
            Ok(ReconciliationResult::Committed(output)) => {
                record.reconciliation = Some(ReconciliationRecord {
                    attempted: true,
                    resolved: true,
                    detail: "recovery reconciliation confirmed commit".to_string(),
                });
                record.commit = CommitRecord {
                    disposition: CommitDisposition::ReconciledCommitted,
                    detail: "commit re-established by recovery reconciliation".to_string(),
                };
                record.failure = None;
                self.transition(&record.execution_id, &mut trace, ExecutionState::Committed)?;
                self.after_commit(action, ctx, &snapshot, output, record, &mut trace)
            }
            Ok(ReconciliationResult::NotCommitted) => {
                record.reconciliation = Some(ReconciliationRecord {
                    attempted: true,
                    resolved: true,
                    detail: "recovery established no commit".to_string(),
                });
                record.commit = CommitRecord {
                    disposition: CommitDisposition::ReconciledNotCommitted,
                    detail: "no effect observed during recovery".to_string(),
                };
                if trace.state() == ExecutionState::ReconciliationRequired
                    && record
                        .recovery
                        .as_ref()
                        .is_some_and(|recovery| recovery.from_state == ExecutionState::Committed)
                {
                    record.failure = Some(FailureRecord {
                        class: FailureClass::RecoveryFailed,
                        message: "durable committed marker conflicts with reconciliation"
                            .to_string(),
                    });
                    return self.persist_checkpoint(
                        record,
                        &trace,
                        ExecutionOutcome::ReconciliationRequired,
                        None,
                    );
                }
                self.finish_terminal(
                    record,
                    &mut trace,
                    ExecutionState::Aborted,
                    ExecutionOutcome::Aborted,
                    None,
                )
            }
            Ok(ReconciliationResult::Unresolved { reason }) => {
                record.reconciliation = Some(ReconciliationRecord {
                    attempted: true,
                    resolved: false,
                    detail: reason.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::ReconciliationFailed,
                    message: reason,
                });
                self.persist_checkpoint(
                    record,
                    &trace,
                    ExecutionOutcome::ReconciliationRequired,
                    None,
                )
            }
            Err(error) => {
                record.reconciliation = Some(ReconciliationRecord {
                    attempted: true,
                    resolved: false,
                    detail: error.to_string(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::ReconciliationFailed,
                    message: error.to_string(),
                });
                self.persist_checkpoint(
                    record,
                    &trace,
                    ExecutionOutcome::ReconciliationRequired,
                    None,
                )
            }
        }
    }

    fn after_commit<A: Action>(
        &self,
        action: &A,
        ctx: &mut A::Context,
        snapshot: &A::Snapshot,
        output: A::Output,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        record.invariants_after = action
            .invariants()
            .into_iter()
            .map(|invariant| invariant.check(InvariantPhase::AfterCommit, ctx))
            .collect();
        if let Some(failed) = record.invariants_after.iter().find(|check| !check.passed) {
            record.failure = Some(FailureRecord {
                class: FailureClass::InvariantViolation,
                message: format!("{}: {}", failed.name, failed.reason),
            });
            return self.rollback_after_failure(action, ctx, snapshot, record, trace);
        }

        self.fault(FaultPoint::BeforeVerify)?;
        match action.verify(ctx, &output) {
            Ok(checks) if checks.iter().all(|check| check.passed) => {
                record.verification = Some(VerificationRecord {
                    passed: true,
                    indeterminate: false,
                    checks,
                    detail: "postconditions verified".to_string(),
                });
            }
            Ok(checks) => {
                let detail = checks.iter().find(|check| !check.passed).map_or_else(
                    || "verification failed".to_string(),
                    |check| format!("{}: {}", check.name, check.reason),
                );
                record.verification = Some(VerificationRecord {
                    passed: false,
                    indeterminate: false,
                    checks,
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::VerificationFailed,
                    message: detail,
                });
                return self.rollback_after_failure(action, ctx, snapshot, record, trace);
            }
            Err(error) => {
                let detail = error.to_string();
                record.verification = Some(VerificationRecord {
                    passed: false,
                    indeterminate: true,
                    checks: Vec::new(),
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::VerificationIndeterminate,
                    message: detail,
                });
                return self.persist_checkpoint(
                    record,
                    trace,
                    ExecutionOutcome::VerificationRequired,
                    Some(output),
                );
            }
        }

        self.transition(&record.execution_id, trace, ExecutionState::Verified)?;
        debug_assert!(record
            .verification
            .as_ref()
            .is_some_and(|value| value.passed && !value.indeterminate));
        debug_assert!(matches!(
            record.commit.disposition,
            CommitDisposition::Confirmed | CommitDisposition::ReconciledCommitted
        ));
        self.finish_terminal(
            record,
            trace,
            ExecutionState::Finalized,
            ExecutionOutcome::Success,
            Some(output),
        )
    }

    fn rollback_after_failure<A: Action>(
        &self,
        action: &A,
        ctx: &mut A::Context,
        snapshot: &A::Snapshot,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        if action.compensation_policy() == CompensationPolicy::NonCompensable {
            record.rollback = Some(RollbackRecord {
                attempted: false,
                succeeded: false,
                verified: false,
                verification_indeterminate: false,
                checks: Vec::new(),
                detail: "action declares no compensating rollback".to_string(),
            });
            record.failure = Some(FailureRecord {
                class: FailureClass::CompensationUnavailable,
                message: "verification failed and action is non-compensable".to_string(),
            });
            return self.finish_terminal(
                record,
                trace,
                ExecutionState::Failed,
                ExecutionOutcome::RollbackFailed,
                None,
            );
        }

        self.transition(&record.execution_id, trace, ExecutionState::RollbackPending)?;
        self.fault(FaultPoint::DuringRollback)?;
        let permit = EffectPermit::new();
        match action.rollback_status(&permit, ctx, snapshot) {
            RollbackStatus::Succeeded => {}
            RollbackStatus::Failed(error) => {
                record.rollback = Some(RollbackRecord {
                    attempted: true,
                    succeeded: false,
                    verified: false,
                    verification_indeterminate: false,
                    checks: Vec::new(),
                    detail: error.to_string(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: error.to_string(),
                });
                return self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                );
            }
            RollbackStatus::Conflict { reason } => {
                record.rollback = Some(RollbackRecord {
                    attempted: true,
                    succeeded: false,
                    verified: false,
                    verification_indeterminate: false,
                    checks: Vec::new(),
                    detail: reason.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::CompensationConflict,
                    message: reason,
                });
                return self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                );
            }
        }
        self.fault(FaultPoint::AfterRollback)?;
        self.verify_rollback(action, ctx, snapshot, record, trace, true)
    }

    fn resume_rollback<A: Action>(
        &self,
        action: &A,
        ctx: &mut A::Context,
        snapshot: &A::Snapshot,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        if action.compensation_policy() == CompensationPolicy::NonCompensable {
            record.failure = Some(FailureRecord {
                class: FailureClass::CompensationUnavailable,
                message: "cannot resume rollback for non-compensable action".to_string(),
            });
            record.rollback = Some(RollbackRecord {
                attempted: false,
                succeeded: false,
                verified: false,
                verification_indeterminate: false,
                checks: Vec::new(),
                detail: "compensation unavailable".to_string(),
            });
            return self.finish_terminal(
                record,
                trace,
                ExecutionState::Failed,
                ExecutionOutcome::RollbackFailed,
                None,
            );
        }

        let precheck = action.verify_rollback(ctx, snapshot);
        let invariants: Vec<_> = action
            .invariants()
            .into_iter()
            .map(|invariant| invariant.check(InvariantPhase::AfterRollback, ctx))
            .collect();
        let invariants_ok = invariants.iter().all(|check| check.passed);
        match precheck {
            Ok(checks) if checks.iter().all(|check| check.passed) && invariants_ok => {
                record.invariants_after_rollback = invariants;
                record.rollback = Some(RollbackRecord {
                    attempted: false,
                    succeeded: true,
                    verified: true,
                    verification_indeterminate: false,
                    checks,
                    detail: "rollback was already complete when recovery resumed".to_string(),
                });
                return self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::RolledBack,
                    ExecutionOutcome::RolledBack,
                    None,
                );
            }
            Err(error) if invariants_ok => {
                let detail = error.to_string();
                record.invariants_after_rollback = invariants;
                record.rollback = Some(RollbackRecord {
                    attempted: false,
                    succeeded: false,
                    verified: false,
                    verification_indeterminate: true,
                    checks: Vec::new(),
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackVerificationIndeterminate,
                    message: detail,
                });
                return self.persist_checkpoint(
                    record,
                    trace,
                    ExecutionOutcome::RollbackVerificationRequired,
                    None,
                );
            }
            Ok(_) | Err(_) => {}
        }

        let permit = EffectPermit::new();
        match action.rollback_status(&permit, ctx, snapshot) {
            RollbackStatus::Succeeded => {}
            RollbackStatus::Failed(error) => {
                record.rollback = Some(RollbackRecord {
                    attempted: true,
                    succeeded: false,
                    verified: false,
                    verification_indeterminate: false,
                    checks: Vec::new(),
                    detail: error.to_string(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: error.to_string(),
                });
                return self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                );
            }
            RollbackStatus::Conflict { reason } => {
                record.rollback = Some(RollbackRecord {
                    attempted: true,
                    succeeded: false,
                    verified: false,
                    verification_indeterminate: false,
                    checks: Vec::new(),
                    detail: reason.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::CompensationConflict,
                    message: reason,
                });
                return self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                );
            }
        }
        self.verify_rollback(action, ctx, snapshot, record, trace, true)
    }

    fn verify_rollback<A: Action>(
        &self,
        action: &A,
        ctx: &A::Context,
        snapshot: &A::Snapshot,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
        attempted: bool,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let verification = action.verify_rollback(ctx, snapshot);
        record.invariants_after_rollback = action
            .invariants()
            .into_iter()
            .map(|invariant| invariant.check(InvariantPhase::AfterRollback, ctx))
            .collect();
        let invariants_ok = record
            .invariants_after_rollback
            .iter()
            .all(|check| check.passed);

        match verification {
            Ok(checks) if checks.iter().all(|check| check.passed) && invariants_ok => {
                record.rollback = Some(RollbackRecord {
                    attempted,
                    succeeded: true,
                    verified: true,
                    verification_indeterminate: false,
                    checks,
                    detail: "rollback verified".to_string(),
                });
                self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::RolledBack,
                    ExecutionOutcome::RolledBack,
                    None,
                )
            }
            Ok(checks) => {
                let detail = if let Some(failed) = checks.iter().find(|check| !check.passed) {
                    format!(
                        "rollback verification failed: {}: {}",
                        failed.name, failed.reason
                    )
                } else if let Some(failed) = record
                    .invariants_after_rollback
                    .iter()
                    .find(|check| !check.passed)
                {
                    format!(
                        "rollback invariant failed: {}: {}",
                        failed.name, failed.reason
                    )
                } else {
                    "rollback verification failed".to_string()
                };
                record.rollback = Some(RollbackRecord {
                    attempted,
                    succeeded: true,
                    verified: false,
                    verification_indeterminate: false,
                    checks,
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: detail,
                });
                self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                )
            }
            Err(error) if invariants_ok => {
                let detail = error.to_string();
                record.rollback = Some(RollbackRecord {
                    attempted,
                    succeeded: true,
                    verified: false,
                    verification_indeterminate: true,
                    checks: Vec::new(),
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackVerificationIndeterminate,
                    message: detail,
                });
                self.persist_checkpoint(
                    record,
                    trace,
                    ExecutionOutcome::RollbackVerificationRequired,
                    None,
                )
            }
            Err(_) => {
                let detail = record
                    .invariants_after_rollback
                    .iter()
                    .find(|check| !check.passed)
                    .map_or_else(
                        || "rollback verification failed".to_string(),
                        |check| {
                            format!(
                                "rollback invariant failed: {}: {}",
                                check.name, check.reason
                            )
                        },
                    );
                record.rollback = Some(RollbackRecord {
                    attempted,
                    succeeded: true,
                    verified: false,
                    verification_indeterminate: false,
                    checks: Vec::new(),
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: detail,
                });
                self.finish_terminal(
                    record,
                    trace,
                    ExecutionState::Failed,
                    ExecutionOutcome::RollbackFailed,
                    None,
                )
            }
        }
    }

    fn decode_snapshot<A: Action>(
        &self,
        envelope: &RecoveryEnvelope,
    ) -> Result<A::Snapshot, ExecutionError> {
        serde_json::from_value(envelope.snapshot.clone())
            .map_err(|error| ExecutionError::RecoveryData(error.to_string()))
    }

    fn rebuild_trace(&self, execution_id: &str) -> Result<ExecutionTrace, ExecutionError> {
        let mut trace = ExecutionTrace::new();
        for entry in self
            .journal
            .entries()
            .map_err(ExecutionError::Journal)?
            .into_iter()
            .filter(|entry| entry.execution_id == execution_id)
        {
            trace.transition(entry.state, entry.at_ms)?;
        }
        Ok(trace)
    }

    fn new_record<A: Action>(
        &self,
        execution_id: String,
        action: &A,
        idempotency_key: Option<String>,
        started_at_ms: u64,
    ) -> ExecutionRecord {
        ExecutionRecord {
            schema_version: 4,
            execution_id,
            action_id: action.action_id(),
            action_type: action.action_type().to_string(),
            idempotency_key,
            started_at_ms,
            completed_at_ms: started_at_ms,
            transitions: Vec::new(),
            validation: Vec::new(),
            preconditions: Vec::new(),
            invariants_before: Vec::new(),
            invariants_after: Vec::new(),
            invariants_after_rollback: Vec::new(),
            commit: CommitRecord {
                disposition: CommitDisposition::NotAttempted,
                detail: String::new(),
            },
            reconciliation: None,
            verification: None,
            rollback: None,
            recovery: None,
            failure: None,
            outcome: ExecutionOutcome::Aborted,
            record_hash: None,
        }
    }

    fn transition(
        &self,
        execution_id: &str,
        trace: &mut ExecutionTrace,
        next: ExecutionState,
    ) -> Result<(), ExecutionError> {
        let at_ms = self.clock.now_ms();
        trace.transition(next, at_ms)?;
        self.journal
            .append(JournalEntry::state(execution_id, next, at_ms))
            .map_err(ExecutionError::Journal)
    }

    fn transition_with_recovery(
        &self,
        execution_id: &str,
        trace: &mut ExecutionTrace,
        next: ExecutionState,
        recovery: RecoveryEnvelope,
    ) -> Result<(), ExecutionError> {
        let at_ms = self.clock.now_ms();
        trace.transition(next, at_ms)?;
        self.journal
            .append(JournalEntry {
                execution_id: execution_id.to_string(),
                state: next,
                at_ms,
                recovery: Some(recovery),
            })
            .map_err(ExecutionError::Journal)
    }

    fn fault(&self, point: FaultPoint) -> Result<(), ExecutionError> {
        self.faults
            .hit(point)
            .map_err(|_| ExecutionError::InjectedFault(format!("{point:?}")))
    }

    fn persist_record(
        &self,
        mut record: ExecutionRecord,
    ) -> Result<ExecutionRecord, ExecutionError> {
        self.redactor.redact(&mut record);
        let record = record
            .seal()
            .map_err(|error| ExecutionError::EvidencePersistence(error.to_string()))?;
        self.evidence
            .persist(&record)
            .map_err(ExecutionError::EvidencePersistence)?;
        Ok(record)
    }

    fn persist_checkpoint<T: Clone>(
        &self,
        mut record: ExecutionRecord,
        trace: &ExecutionTrace,
        outcome: ExecutionOutcome,
        output: Option<T>,
    ) -> Result<ExecutionResult<T>, ExecutionError> {
        record.completed_at_ms = self.clock.now_ms();
        record.transitions = trace.transitions().to_vec();
        record.outcome = outcome;
        let record = self.persist_record(record)?;
        Ok(ExecutionResult {
            execution_id: record.execution_id.clone(),
            outcome,
            output,
            record,
        })
    }

    fn finish_terminal<T: Clone>(
        &self,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
        terminal: ExecutionState,
        outcome: ExecutionOutcome,
        output: Option<T>,
    ) -> Result<ExecutionResult<T>, ExecutionError> {
        debug_assert!(terminal.is_terminal());
        let at_ms = self.clock.now_ms();
        trace.transition(terminal, at_ms)?;
        record.completed_at_ms = at_ms;
        record.transitions = trace.transitions().to_vec();
        record.outcome = outcome;
        let record = self.persist_record(record)?;

        // K5 ordering: terminal state becomes durable only after its evidence exists.
        self.journal
            .append(JournalEntry::state(&record.execution_id, terminal, at_ms))
            .map_err(ExecutionError::Journal)?;

        Ok(ExecutionResult {
            execution_id: record.execution_id.clone(),
            outcome,
            output,
            record,
        })
    }
}
