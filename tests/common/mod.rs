use execution_assurance_microkernel::{
    Action, CheckRecord, Clock, CommitStatus, EvidenceStore, FaultInjector, IdGenerator,
    IdempotencyStore, InMemoryEvidenceStore, InMemoryIdempotencyStore, InMemoryJournal, Invariant,
    InvariantPhase, Journal, Kernel, NoFaultInjector, Predicate, ReconciliationResult,
    SequenceIdGenerator,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use thiserror::Error;

#[derive(Debug, Default)]
pub struct World {
    pub value: i32,
    pub commits: usize,
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct TestError(pub &'static str);

#[derive(Debug)]
struct Allowed(pub bool);

impl Predicate<World> for Allowed {
    fn evaluate(&self, _ctx: &World) -> CheckRecord {
        if self.0 {
            CheckRecord::pass("allowed", "precondition passed")
        } else {
            CheckRecord::fail("allowed", "precondition denied")
        }
    }
}

#[derive(Debug)]
struct NonNegative;

impl Invariant<World> for NonNegative {
    fn check(&self, _phase: InvariantPhase, ctx: &World) -> CheckRecord {
        if ctx.value >= 0 {
            CheckRecord::pass("non_negative", format!("value={}", ctx.value))
        } else {
            CheckRecord::fail("non_negative", format!("value={}", ctx.value))
        }
    }
}

#[derive(Debug, Clone)]
pub enum CommitBehavior {
    Confirmed,
    Failed,
    UnknownCommitted,
    UnknownUnresolved,
}

#[derive(Debug, Clone)]
pub struct TestAction {
    pub delta: i32,
    pub allowed: bool,
    pub verify_ok: bool,
    pub rollback_ok: bool,
    pub commit_behavior: CommitBehavior,
}

impl Default for TestAction {
    fn default() -> Self {
        Self {
            delta: 1,
            allowed: true,
            verify_ok: true,
            rollback_ok: true,
            commit_behavior: CommitBehavior::Confirmed,
        }
    }
}

impl Action for TestAction {
    type Context = World;
    type Output = i32;
    type Error = TestError;
    type Snapshot = i32;

    fn action_id(&self) -> String {
        "test-action".to_string()
    }
    fn action_type(&self) -> &'static str {
        "test_action"
    }
    fn validate(&self, _ctx: &World) -> Result<(), Self::Error> {
        Ok(())
    }

    fn preconditions(&self) -> Vec<Box<dyn Predicate<World> + '_>> {
        vec![Box::new(Allowed(self.allowed))]
    }

    fn invariants(&self) -> Vec<Box<dyn Invariant<World> + '_>> {
        vec![Box::new(NonNegative)]
    }

    fn snapshot(&self, ctx: &World) -> Result<Self::Snapshot, Self::Error> {
        Ok(ctx.value)
    }

    fn commit(&self, ctx: &mut World) -> CommitStatus<Self::Output, Self::Error> {
        match self.commit_behavior {
            CommitBehavior::Failed => CommitStatus::Failed(TestError("commit failed")),
            CommitBehavior::Confirmed => {
                ctx.commits += 1;
                ctx.value += self.delta;
                CommitStatus::Confirmed(ctx.value)
            }
            CommitBehavior::UnknownCommitted | CommitBehavior::UnknownUnresolved => {
                ctx.commits += 1;
                ctx.value += self.delta;
                CommitStatus::Unknown {
                    reason: "response lost".to_string(),
                }
            }
        }
    }

    fn reconcile(
        &self,
        ctx: &mut World,
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        Ok(match self.commit_behavior {
            CommitBehavior::UnknownCommitted => ReconciliationResult::Committed(ctx.value),
            CommitBehavior::UnknownUnresolved => ReconciliationResult::Unresolved {
                reason: "still uncertain".to_string(),
            },
            CommitBehavior::Confirmed | CommitBehavior::Failed => {
                ReconciliationResult::NotCommitted
            }
        })
    }

    fn verify(&self, ctx: &World, output: &Self::Output) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if self.verify_ok && ctx.value == *output {
            CheckRecord::pass("value_persisted", ctx.value.to_string())
        } else {
            CheckRecord::fail("value_persisted", "readback mismatch")
        }])
    }

    fn rollback(&self, ctx: &mut World, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
        if !self.rollback_ok {
            return Err(TestError("rollback failed"));
        }
        ctx.value = *snapshot;
        Ok(())
    }

    fn verify_rollback(
        &self,
        ctx: &World,
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if ctx.value == *snapshot {
            CheckRecord::pass("rollback_value", ctx.value.to_string())
        } else {
            CheckRecord::fail("rollback_value", "snapshot not restored")
        }])
    }
}

#[derive(Debug, Default)]
pub struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

pub fn kernel_with(
    journal: Arc<dyn Journal>,
    evidence: Arc<dyn EvidenceStore>,
    idempotency: Arc<dyn IdempotencyStore>,
    faults: Arc<dyn FaultInjector>,
) -> Kernel {
    let clock: Arc<dyn Clock> = Arc::new(TestClock::default());
    let ids: Arc<dyn IdGenerator> = Arc::new(SequenceIdGenerator::starting_at(1));
    Kernel::with_components(journal, evidence, idempotency, faults, clock, ids)
}

pub fn basic_kernel() -> Kernel {
    kernel_with(
        Arc::new(InMemoryJournal::default()),
        Arc::new(InMemoryEvidenceStore::default()),
        Arc::new(InMemoryIdempotencyStore::default()),
        Arc::new(NoFaultInjector),
    )
}
