use crate::error::ExecutionError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Created,
    Proposed,
    Validated,
    Prepared,
    Committed,
    ReconciliationRequired,
    RollbackPending,
    RolledBack,
    Verified,
    Finalized,
    Rejected,
    Aborted,
    Failed,
}

impl ExecutionState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::Rejected | Self::Aborted | Self::RolledBack | Self::Failed
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use ExecutionState::{
            Aborted, Committed, Created, Failed, Finalized, Prepared, Proposed,
            ReconciliationRequired, Rejected, RollbackPending, RolledBack, Validated, Verified,
        };
        matches!(
            (self, next),
            (Created, Proposed)
                | (Proposed, Validated)
                | (Proposed, Rejected)
                | (Validated, Prepared)
                | (Validated, Aborted)
                | (Prepared, Committed)
                | (Prepared, Failed)
                | (Prepared, ReconciliationRequired)
                | (Committed, ReconciliationRequired)
                | (ReconciliationRequired, Committed)
                | (ReconciliationRequired, Aborted)
                | (Committed, Verified)
                | (Committed, RollbackPending)
                | (Committed, Failed)
                | (Verified, Finalized)
                | (RollbackPending, RolledBack)
                | (RollbackPending, Failed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: ExecutionState,
    pub to: ExecutionState,
    pub at_ms: u64,
}

#[derive(Debug)]
pub(crate) struct ExecutionTrace {
    state: ExecutionState,
    transitions: Vec<StateTransition>,
}

impl ExecutionTrace {
    pub(crate) fn new() -> Self {
        Self {
            state: ExecutionState::Created,
            transitions: Vec::new(),
        }
    }

    pub(crate) fn state(&self) -> ExecutionState {
        self.state
    }

    pub(crate) fn transitions(&self) -> &[StateTransition] {
        &self.transitions
    }

    pub(crate) fn transition(
        &mut self,
        next: ExecutionState,
        at_ms: u64,
    ) -> Result<(), ExecutionError> {
        if !self.state.can_transition_to(next) {
            return Err(ExecutionError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        let previous = self.state;
        self.state = next;
        self.transitions.push(StateTransition {
            from: previous,
            to: next,
            at_ms,
        });
        Ok(())
    }
}
