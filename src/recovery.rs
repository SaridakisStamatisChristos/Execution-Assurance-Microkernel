use crate::{ExecutionState, Journal, RecoveryEnvelope};
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

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryDirective {
    pub execution_id: String,
    pub last_state: ExecutionState,
    pub plan: RecoveryPlan,
    pub recovery: Option<RecoveryEnvelope>,
}

#[derive(Debug, Default)]
pub struct RecoveryManager;

impl RecoveryManager {
    pub fn classify(state: ExecutionState) -> RecoveryPlan {
        match state {
            ExecutionState::Created | ExecutionState::Proposed | ExecutionState::Validated => {
                RecoveryPlan::AbortBeforeCommit
            }
            ExecutionState::Prepared | ExecutionState::ReconciliationRequired => {
                RecoveryPlan::ReconcileBeforeRetry
            }
            ExecutionState::Committed => RecoveryPlan::VerifyOrRollback,
            ExecutionState::RollbackPending => RecoveryPlan::CompleteRollback,
            ExecutionState::Verified => RecoveryPlan::FinalizeVerified,
            ExecutionState::Finalized
            | ExecutionState::Rejected
            | ExecutionState::Aborted
            | ExecutionState::RolledBack
            | ExecutionState::Failed => RecoveryPlan::None,
        }
    }

    pub fn scan(journal: &dyn Journal) -> Result<Vec<RecoveryDirective>, String> {
        let mut states = BTreeMap::new();
        let mut recovery = BTreeMap::new();
        for entry in journal.entries()? {
            if let Some(envelope) = entry.recovery.clone() {
                recovery.insert(entry.execution_id.clone(), envelope);
            }
            states.insert(entry.execution_id.clone(), entry.state);
        }
        Ok(states
            .into_iter()
            .map(|(execution_id, last_state)| RecoveryDirective {
                recovery: recovery.remove(&execution_id),
                plan: Self::classify(last_state),
                execution_id,
                last_state,
            })
            .filter(|directive| directive.plan != RecoveryPlan::None)
            .collect())
    }

    pub fn directive(
        journal: &dyn Journal,
        execution_id: &str,
    ) -> Result<Option<RecoveryDirective>, String> {
        Ok(Self::scan(journal)?
            .into_iter()
            .find(|directive| directive.execution_id == execution_id))
    }
}
