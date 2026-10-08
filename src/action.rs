use crate::invariant::{Invariant, Predicate};

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

pub trait Action {
    type Context;
    type Output: Clone + std::fmt::Debug + serde::Serialize;
    type Error: std::error::Error + Send + Sync + 'static;
    type Snapshot: Clone + std::fmt::Debug;

    fn action_id(&self) -> String;
    fn action_type(&self) -> &'static str;
    fn validate(&self, ctx: &Self::Context) -> Result<(), Self::Error>;

    fn preconditions(&self) -> Vec<Box<dyn Predicate<Self::Context> + '_>> {
        Vec::new()
    }

    fn invariants(&self) -> Vec<Box<dyn Invariant<Self::Context> + '_>> {
        Vec::new()
    }

    fn snapshot(&self, ctx: &Self::Context) -> Result<Self::Snapshot, Self::Error>;
    fn commit(&self, ctx: &mut Self::Context) -> CommitStatus<Self::Output, Self::Error>;

    fn verify(
        &self,
        ctx: &Self::Context,
        output: &Self::Output,
    ) -> Result<Vec<crate::CheckRecord>, Self::Error>;

    fn rollback(
        &self,
        ctx: &mut Self::Context,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Self::Error>;

    fn verify_rollback(
        &self,
        ctx: &Self::Context,
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<crate::CheckRecord>, Self::Error>;

    fn reconcile(
        &self,
        _ctx: &mut Self::Context,
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        Ok(ReconciliationResult::Unresolved {
            reason: "no reconciliation strategy supplied".to_string(),
        })
    }
}
