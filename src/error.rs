use crate::state::ExecutionState;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ValidationFailed,
    PreconditionFailed,
    SnapshotFailed,
    CommitFailed,
    CommitOutcomeUnknown,
    VerificationFailed,
    InvariantViolation,
    RollbackFailed,
    CompensationUnavailable,
    ReconciliationFailed,
    RecoveryFailed,
    EvidencePersistenceFailed,
    JournalWriteFailed,
    ExecutionIdConflict,
    IdempotencyConflict,
    InjectedFault,
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: ExecutionState,
        to: ExecutionState,
    },
    #[error("journal failure: {0}")]
    Journal(String),
    #[error("evidence persistence failure: {0}")]
    EvidencePersistence(String),
    #[error("idempotency store failure: {0}")]
    Idempotency(String),
    #[error("duplicate execution id: {0}")]
    DuplicateExecutionId(String),
    #[error("recovery unavailable for execution: {0}")]
    RecoveryUnavailable(String),
    #[error("recovery action mismatch: {0}")]
    RecoveryMismatch(String),
    #[error("recovery data failure: {0}")]
    RecoveryData(String),
    #[error("execution already has terminal evidence: {0}")]
    AlreadyFinalized(String),
    #[error("fault injected at {0}")]
    InjectedFault(String),
}
