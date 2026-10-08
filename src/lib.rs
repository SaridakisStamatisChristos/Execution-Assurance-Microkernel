//! Execution Assurance Microkernel.
//!
//! The core semantic rule is deliberately narrow:
//! **a commit is not success; success requires a verified postcondition.**

pub mod action;
pub mod error;
pub mod evidence;
pub mod fault;
pub mod idempotency;
pub mod invariant;
pub mod journal;
pub mod kernel;
pub mod recovery;
pub mod state;

pub use action::{Action, CommitStatus, ReconciliationResult};
pub use error::{ExecutionError, FailureClass};
pub use evidence::{
    CheckRecord, CommitDisposition, EvidenceStore, ExecutionOutcome, ExecutionRecord,
    ExecutionResult, InMemoryEvidenceStore, JsonlEvidenceStore, ReconciliationRecord,
    RollbackRecord, VerificationRecord,
};
pub use fault::{FaultInjector, FaultPoint, NoFaultInjector, ScriptedFaultInjector};
pub use idempotency::{IdempotencyKey, IdempotencyStore, InMemoryIdempotencyStore};
pub use invariant::{Invariant, InvariantPhase, Predicate};
pub use journal::{FileJournal, InMemoryJournal, Journal, JournalEntry};
pub use kernel::{Clock, IdGenerator, Kernel, SequenceIdGenerator, SystemClock, UuidIdGenerator};
pub use recovery::{RecoveryDirective, RecoveryManager, RecoveryPlan};
pub use state::{ExecutionState, StateTransition};
