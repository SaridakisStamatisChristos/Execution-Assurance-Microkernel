use crate::evidence::CheckRecord;
use serde::{Deserialize, Serialize};

pub trait Predicate<C>: Send + Sync {
    fn evaluate(&self, ctx: &C) -> CheckRecord;
}

pub trait Invariant<C>: Send + Sync {
    fn check(&self, phase: InvariantPhase, ctx: &C) -> CheckRecord;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantPhase {
    BeforeCommit,
    AfterCommit,
    AfterRollback,
}
