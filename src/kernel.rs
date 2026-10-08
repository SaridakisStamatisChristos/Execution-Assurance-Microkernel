use crate::{
    action::{Action, CommitStatus, ReconciliationResult},
    error::{ExecutionError, FailureClass},
    evidence::{
        CheckRecord, CommitDisposition, CommitRecord, EvidenceStore, ExecutionOutcome,
        ExecutionRecord, ExecutionResult, FailureRecord, InMemoryEvidenceStore,
        ReconciliationRecord, RollbackRecord, VerificationRecord,
    },
    fault::{FaultInjector, FaultPoint, NoFaultInjector},
    idempotency::{ClaimOutcome, IdempotencyKey, IdempotencyStore, InMemoryIdempotencyStore},
    invariant::InvariantPhase,
    journal::{InMemoryJournal, Journal, JournalEntry},
    state::{ExecutionState, ExecutionTrace},
};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
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

#[derive(Clone)]
pub struct Kernel {
    journal: Arc<dyn Journal>,
    evidence: Arc<dyn EvidenceStore>,
    idempotency: Arc<dyn IdempotencyStore>,
    faults: Arc<dyn FaultInjector>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
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
        Self {
            journal,
            evidence,
            idempotency,
            faults,
            clock,
            ids,
        }
    }

    pub fn execute<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        self.execute_with_key(action, ctx, None)
    }

    pub fn execute_idempotent<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
        key: IdempotencyKey,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        self.execute_with_key(action, ctx, Some(key))
    }

    fn execute_with_key<A: Action>(
        &self,
        action: A,
        ctx: &mut A::Context,
        key: Option<IdempotencyKey>,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let execution_id = self.ids.next_id();
        let started = self.clock.now_ms();
        let mut trace = ExecutionTrace::new();
        let mut record = ExecutionRecord {
            schema_version: 1,
            execution_id: execution_id.clone(),
            action_id: action.action_id(),
            action_type: action.action_type().to_string(),
            idempotency_key: key.as_ref().map(|key| key.0.clone()),
            started_at_ms: started,
            completed_at_ms: started,
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
            failure: None,
            outcome: ExecutionOutcome::Aborted,
            record_hash: None,
        };

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
                    self.transition(&execution_id, &mut trace, ExecutionState::Rejected)?;
                    return self.finalize(record, &trace, ExecutionOutcome::Rejected, None);
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
                self.transition(&execution_id, &mut trace, ExecutionState::Rejected)?;
                return self.finalize(record, &trace, ExecutionOutcome::Rejected, None);
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
            self.transition(&execution_id, &mut trace, ExecutionState::Rejected)?;
            return self.finalize(record, &trace, ExecutionOutcome::Rejected, None);
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
                self.transition(&execution_id, &mut trace, ExecutionState::Aborted)?;
                return self.finalize(record, &trace, ExecutionOutcome::Aborted, None);
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
            self.transition(&execution_id, &mut trace, ExecutionState::Aborted)?;
            return self.finalize(record, &trace, ExecutionOutcome::Aborted, None);
        }

        self.transition(&execution_id, &mut trace, ExecutionState::Prepared)?;
        self.fault(FaultPoint::BeforeCommit)?;
        self.fault(FaultPoint::DuringCommit)?;

        let output = match action.commit(ctx) {
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
                self.transition(&execution_id, &mut trace, ExecutionState::Failed)?;
                return self.finalize(record, &trace, ExecutionOutcome::CommitFailed, None);
            }
            CommitStatus::Unknown { reason } => {
                record.commit = CommitRecord {
                    disposition: CommitDisposition::Unknown,
                    detail: reason.clone(),
                };
                record.failure = Some(FailureRecord {
                    class: FailureClass::CommitOutcomeUnknown,
                    message: reason.clone(),
                });
                self.transition(
                    &execution_id,
                    &mut trace,
                    ExecutionState::ReconciliationRequired,
                )?;
                match action.reconcile(ctx) {
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
                            detail: "no effect observed".to_string(),
                        };
                        self.transition(&execution_id, &mut trace, ExecutionState::Aborted)?;
                        return self.finalize(record, &trace, ExecutionOutcome::Aborted, None);
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
                        return self.finalize(
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
                        return self.finalize(
                            record,
                            &trace,
                            ExecutionOutcome::ReconciliationRequired,
                            None,
                        );
                    }
                }
            }
        };

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
            return self.rollback_after_failure(&action, ctx, &snapshot, record, &mut trace);
        }

        self.fault(FaultPoint::BeforeVerify)?;
        match action.verify(ctx, &output) {
            Ok(checks) if checks.iter().all(|check| check.passed) => {
                record.verification = Some(VerificationRecord {
                    passed: true,
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
                    checks,
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::VerificationFailed,
                    message: detail,
                });
                return self.rollback_after_failure(&action, ctx, &snapshot, record, &mut trace);
            }
            Err(error) => {
                record.verification = Some(VerificationRecord {
                    passed: false,
                    checks: Vec::new(),
                    detail: error.to_string(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::VerificationFailed,
                    message: error.to_string(),
                });
                return self.rollback_after_failure(&action, ctx, &snapshot, record, &mut trace);
            }
        }

        self.transition(&execution_id, &mut trace, ExecutionState::Verified)?;
        self.transition(&execution_id, &mut trace, ExecutionState::Finalized)?;
        self.finalize(record, &trace, ExecutionOutcome::Success, Some(output))
    }

    fn rollback_after_failure<A: Action>(
        &self,
        action: &A,
        ctx: &mut A::Context,
        snapshot: &A::Snapshot,
        mut record: ExecutionRecord,
        trace: &mut ExecutionTrace,
    ) -> Result<ExecutionResult<A::Output>, ExecutionError> {
        let execution_id = record.execution_id.clone();
        self.transition(&execution_id, trace, ExecutionState::RollbackPending)?;
        self.fault(FaultPoint::DuringRollback)?;

        if let Err(error) = action.rollback(ctx, snapshot) {
            record.rollback = Some(RollbackRecord {
                attempted: true,
                succeeded: false,
                verified: false,
                checks: Vec::new(),
                detail: error.to_string(),
            });
            record.failure = Some(FailureRecord {
                class: FailureClass::RollbackFailed,
                message: error.to_string(),
            });
            self.transition(&execution_id, trace, ExecutionState::Failed)?;
            return self.finalize(record, trace, ExecutionOutcome::RollbackFailed, None);
        }

        self.fault(FaultPoint::AfterRollback)?;
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
                    attempted: true,
                    succeeded: true,
                    verified: true,
                    checks,
                    detail: "rollback verified".to_string(),
                });
                self.transition(&execution_id, trace, ExecutionState::RolledBack)?;
                self.finalize(record, trace, ExecutionOutcome::RolledBack, None)
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
                    attempted: true,
                    succeeded: true,
                    verified: false,
                    checks,
                    detail: detail.clone(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: detail,
                });
                self.transition(&execution_id, trace, ExecutionState::Failed)?;
                self.finalize(record, trace, ExecutionOutcome::RollbackFailed, None)
            }
            Err(error) => {
                record.rollback = Some(RollbackRecord {
                    attempted: true,
                    succeeded: true,
                    verified: false,
                    checks: Vec::new(),
                    detail: error.to_string(),
                });
                record.failure = Some(FailureRecord {
                    class: FailureClass::RollbackFailed,
                    message: error.to_string(),
                });
                self.transition(&execution_id, trace, ExecutionState::Failed)?;
                self.finalize(record, trace, ExecutionOutcome::RollbackFailed, None)
            }
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
            .append(JournalEntry {
                execution_id: execution_id.to_string(),
                state: next,
                at_ms,
            })
            .map_err(ExecutionError::Journal)
    }

    fn fault(&self, point: FaultPoint) -> Result<(), ExecutionError> {
        self.faults
            .hit(point)
            .map_err(|_| ExecutionError::InjectedFault(format!("{point:?}")))
    }

    fn finalize<T: Clone>(
        &self,
        mut record: ExecutionRecord,
        trace: &ExecutionTrace,
        outcome: ExecutionOutcome,
        output: Option<T>,
    ) -> Result<ExecutionResult<T>, ExecutionError> {
        record.completed_at_ms = self.clock.now_ms();
        record.transitions = trace.transitions().to_vec();
        record.outcome = outcome;
        let record = record
            .seal()
            .map_err(|error| ExecutionError::EvidencePersistence(error.to_string()))?;
        self.evidence
            .persist(&record)
            .map_err(ExecutionError::EvidencePersistence)?;
        Ok(ExecutionResult {
            execution_id: record.execution_id.clone(),
            outcome,
            output,
            record,
        })
    }
}
