use execution_assurance_microkernel::{
    Action, CheckRecord, CommitStatus, EffectPermit, Kernel, ReconciliationResult,
};
use std::{collections::HashMap, convert::Infallible};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteMode {
    Normal,
    TimeoutBeforeApply,
    LostResponseAfterApply,
    PartialWrite,
}

#[derive(Debug, Default)]
struct FakeApi {
    resources: HashMap<String, String>,
    requests: usize,
    writes: usize,
}

#[derive(Debug, Clone)]
struct CreateResource {
    id: String,
    body: String,
    mode: RemoteMode,
}

impl Action for CreateResource {
    type Context = FakeApi;
    type Output = String;
    type Error = Infallible;
    type Snapshot = Option<String>;

    fn action_id(&self) -> String {
        format!("remote:{}", self.id)
    }

    fn action_type(&self) -> &'static str {
        "fake_remote_create"
    }

    fn validate(&self, _ctx: &FakeApi) -> Result<(), Self::Error> {
        Ok(())
    }

    fn snapshot(&self, api: &FakeApi) -> Result<Self::Snapshot, Self::Error> {
        Ok(api.resources.get(&self.id).cloned())
    }

    fn commit(
        &self,
        _permit: &EffectPermit,
        api: &mut FakeApi,
    ) -> CommitStatus<Self::Output, Self::Error> {
        api.requests += 1;

        // The fake service treats an identical create as a server-side duplicate,
        // returning the same resource identity without applying a second write.
        if api.resources.get(&self.id) == Some(&self.body) {
            return CommitStatus::Confirmed(self.id.clone());
        }

        match self.mode {
            RemoteMode::Normal => {
                api.resources.insert(self.id.clone(), self.body.clone());
                api.writes += 1;
                CommitStatus::Confirmed(self.id.clone())
            }
            RemoteMode::TimeoutBeforeApply => CommitStatus::Unknown {
                reason: "request timed out before the service exposed an outcome".to_string(),
            },
            RemoteMode::LostResponseAfterApply => {
                api.resources.insert(self.id.clone(), self.body.clone());
                api.writes += 1;
                CommitStatus::Unknown {
                    reason: "response lost after server processed request".to_string(),
                }
            }
            RemoteMode::PartialWrite => {
                api.resources
                    .insert(self.id.clone(), format!("{}:partial", self.body));
                api.writes += 1;
                CommitStatus::Confirmed(self.id.clone())
            }
        }
    }

    fn reconcile(
        &self,
        _permit: &EffectPermit,
        api: &mut FakeApi,
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        Ok(match api.resources.get(&self.id) {
            Some(body) if body == &self.body => ReconciliationResult::Committed(self.id.clone()),
            None => ReconciliationResult::NotCommitted,
            Some(_) => ReconciliationResult::Unresolved {
                reason: "identifier exists with unexpected content".to_string(),
            },
        })
    }

    fn verify(
        &self,
        api: &FakeApi,
        output: &Self::Output,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![
            if output == &self.id && api.resources.get(&self.id) == Some(&self.body) {
                CheckRecord::pass(
                    "remote_readback",
                    "resource persisted with expected content",
                )
            } else {
                CheckRecord::fail("remote_readback", "resource does not match")
            },
        ])
    }

    fn rollback(
        &self,
        _permit: &EffectPermit,
        api: &mut FakeApi,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Self::Error> {
        match snapshot {
            Some(body) => {
                api.resources.insert(self.id.clone(), body.clone());
            }
            None => {
                api.resources.remove(&self.id);
            }
        }
        Ok(())
    }

    fn verify_rollback(
        &self,
        api: &FakeApi,
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        let restored = api.resources.get(&self.id).cloned() == *snapshot;
        Ok(vec![if restored {
            CheckRecord::pass("rollback_readback", "remote state restored")
        } else {
            CheckRecord::fail("rollback_readback", "remote state differs from snapshot")
        }])
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut api = FakeApi::default();
    let action = CreateResource {
        id: "r-42".to_string(),
        body: "payload".to_string(),
        mode: RemoteMode::LostResponseAfterApply,
    };
    let result = Kernel::default().execute(action, &mut api)?;
    println!(
        "outcome={:?} reconciliation={:?}",
        result.outcome, result.record.reconciliation
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use execution_assurance_microkernel::ExecutionOutcome;

    fn action(mode: RemoteMode) -> CreateResource {
        CreateResource {
            id: "r-42".to_string(),
            body: "payload".to_string(),
            mode,
        }
    }

    #[test]
    fn lost_response_after_apply_reconciles_to_verified_success() {
        let mut api = FakeApi::default();
        let result = Kernel::default()
            .execute(action(RemoteMode::LostResponseAfterApply), &mut api)
            .unwrap();

        assert_eq!(result.outcome, ExecutionOutcome::Success);
        assert_eq!(api.requests, 1);
        assert_eq!(api.writes, 1);
        assert_eq!(api.resources.get("r-42"), Some(&"payload".to_string()));
        assert!(result.record.reconciliation.unwrap().resolved);
    }

    #[test]
    fn timeout_before_apply_reconciles_to_aborted_without_retry() {
        let mut api = FakeApi::default();
        let result = Kernel::default()
            .execute(action(RemoteMode::TimeoutBeforeApply), &mut api)
            .unwrap();

        assert_eq!(result.outcome, ExecutionOutcome::Aborted);
        assert_eq!(api.requests, 1);
        assert_eq!(api.writes, 0);
        assert!(!api.resources.contains_key("r-42"));
    }

    #[test]
    fn partial_external_write_is_detected_and_rolled_back() {
        let mut api = FakeApi::default();
        let result = Kernel::default()
            .execute(action(RemoteMode::PartialWrite), &mut api)
            .unwrap();

        assert_eq!(result.outcome, ExecutionOutcome::RolledBack);
        assert_eq!(api.requests, 1);
        assert_eq!(api.writes, 1);
        assert!(!api.resources.contains_key("r-42"));
        assert!(result.record.rollback.unwrap().verified);
    }

    #[test]
    fn duplicate_remote_create_is_server_idempotent() {
        let mut api = FakeApi::default();
        Kernel::default()
            .execute(action(RemoteMode::Normal), &mut api)
            .unwrap();
        Kernel::default()
            .execute(action(RemoteMode::Normal), &mut api)
            .unwrap();

        assert_eq!(api.requests, 2);
        assert_eq!(api.writes, 1);
        assert_eq!(api.resources.len(), 1);
    }

    #[test]
    fn conflicting_remote_state_keeps_unknown_outcome_unresolved() {
        let mut api = FakeApi::default();
        api.resources
            .insert("r-42".to_string(), "unexpected".to_string());
        let result = Kernel::default()
            .execute(action(RemoteMode::TimeoutBeforeApply), &mut api)
            .unwrap();

        assert_eq!(result.outcome, ExecutionOutcome::ReconciliationRequired);
        assert_eq!(api.requests, 1);
        assert_eq!(api.writes, 0);
        assert!(!result.record.reconciliation.unwrap().resolved);
    }
}
