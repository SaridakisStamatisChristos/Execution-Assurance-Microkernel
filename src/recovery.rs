use crate::{ExecutionState, Journal};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPlan {
    None,
    AbortBeforeCommit,
    ReconcileBeforeRetry,
    VerifyOrRollback,
    CompleteRollback,
    FinalizeVerified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDirective {
    pub execution_id: String,
    pub last_state: ExecutionState,
    pub plan: RecoveryPlan,
}

#[derive(Debug, Default)]
pub struct RecoveryManager;

impl RecoveryManager {
    pub fn classify(state: ExecutionState) -> RecoveryPlan {
        match state {
            ExecutionState::Created | ExecutionState::Proposed | ExecutionState::Validated => {
                RecoveryPlan::AbortBeforeCommit
            }
            ExecutionState::Prepared => RecoveryPlan::ReconcileBeforeRetry,
            ExecutionState::Committed => RecoveryPlan::VerifyOrRollback,
            ExecutionState::RollbackPending => RecoveryPlan::CompleteRollback,
            ExecutionState::Verified => RecoveryPlan::FinalizeVerified,
            ExecutionState::Finalized
            | ExecutionState::Rejected
            | ExecutionState::Aborted
            | ExecutionState::RolledBack
            | ExecutionState::Failed
            | ExecutionState::ReconciliationRequired => RecoveryPlan::None,
        }
    }

    pub fn scan(journal: &dyn Journal) -> Result<Vec<RecoveryDirective>, String> {
        let mut last = BTreeMap::new();
        for entry in journal.entries()? {
            last.insert(entry.execution_id.clone(), entry.state);
        }
        Ok(last
            .into_iter()
            .map(|(execution_id, last_state)| RecoveryDirective {
                execution_id,
                last_state,
                plan: Self::classify(last_state),
            })
            .filter(|directive| directive.plan != RecoveryPlan::None)
            .collect())
    }
}
