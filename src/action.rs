use crate::invariant::{Invariant, Predicate};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Capability token supplied by `Kernel` when it executes effectful lifecycle hooks.
///
/// Downstream safe code can name this type to implement [`Action`], but cannot
/// construct one because its field is crate-private. This keeps `commit`,
/// `rollback`, and `reconcile` behind the kernel execution boundary.
///
/// ```compile_fail
/// use execution_assurance_microkernel::EffectPermit;
/// let _permit = EffectPermit { _private: () };
/// ```
#[derive(Debug)]
pub struct EffectPermit {
    _private: (),
}

impl EffectPermit {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompensationPolicy {
    Compensable,
    NonCompensable,
}

#[derive(Debug)]
pub enum CommitStatus<T, E> {
    Confirmed(T),
    Failed(E),
    Unknown { reason: String },
}

#[derive(Debug)]
pub enum ReconciliationResult<T> {
    Committed(T),
    NotCommitted,
    Unresolved { reason: String },
}

/// Result of attempting compensation.
///
/// `Conflict` is semantically distinct from an implementation failure: the
/// action deliberately refused to restore its snapshot because the current
/// external state can no longer be attributed to this execution.
#[derive(Debug)]
pub enum RollbackStatus<E> {
    Succeeded,
    Failed(E),
    Conflict { reason: String },
}

pub trait Action {
    type Context;
    type Output: Clone + std::fmt::Debug + Serialize;
    type Error: std::error::Error + Send + Sync + 'static;
    type Snapshot: Clone + std::fmt::Debug + Serialize + DeserializeOwned;

    fn action_id(&self) -> String;
    fn action_type(&self) -> &'static str;
    fn validate(&self, ctx: &Self::Context) -> Result<(), Self::Error>;

    fn preconditions(&self) -> Vec<Box<dyn Predicate<Self::Context> + '_>> {
        Vec::new()
    }

    fn invariants(&self) -> Vec<Box<dyn Invariant<Self::Context> + '_>> {
        Vec::new()
    }

    fn compensation_policy(&self) -> CompensationPolicy {
        CompensationPolicy::Compensable
    }

    fn snapshot(&self, ctx: &Self::Context) -> Result<Self::Snapshot, Self::Error>;

    fn commit(
        &self,
        permit: &EffectPermit,
        ctx: &mut Self::Context,
    ) -> CommitStatus<Self::Output, Self::Error>;

    fn verify(
        &self,
        ctx: &Self::Context,
        output: &Self::Output,
    ) -> Result<Vec<crate::CheckRecord>, Self::Error>;

    fn rollback(
        &self,
        permit: &EffectPermit,
        ctx: &mut Self::Context,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Self::Error>;

    /// Compensation hook with explicit conflict semantics.
    ///
    /// Existing actions that only implement [`Action::rollback`] retain the
    /// original behavior. Actions that can prove ownership of the state they
    /// are about to restore may override this method and return `Conflict`
    /// rather than overwriting state changed by another actor.
    fn rollback_status(
        &self,
        permit: &EffectPermit,
        ctx: &mut Self::Context,
        snapshot: &Self::Snapshot,
    ) -> RollbackStatus<Self::Error> {
        match self.rollback(permit, ctx, snapshot) {
            Ok(()) => RollbackStatus::Succeeded,
            Err(error) => RollbackStatus::Failed(error),
        }
    }

    fn verify_rollback(
        &self,
        ctx: &Self::Context,
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<crate::CheckRecord>, Self::Error>;

    fn reconcile(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut Self::Context,
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        Ok(ReconciliationResult::Unresolved {
            reason: "no reconciliation strategy supplied".to_string(),
        })
    }
}
