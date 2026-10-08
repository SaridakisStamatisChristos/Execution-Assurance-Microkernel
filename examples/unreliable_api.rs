use execution_assurance_microkernel::{
    Action, CheckRecord, CommitStatus, Kernel, ReconciliationResult,
};
use std::{collections::HashMap, convert::Infallible};

#[derive(Debug, Default)]
struct FakeApi {
    resources: HashMap<String, String>,
    lose_next_response: bool,
}

#[derive(Debug)]
struct CreateResource {
    id: String,
    body: String,
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

    fn commit(&self, api: &mut FakeApi) -> CommitStatus<Self::Output, Self::Error> {
        api.resources.insert(self.id.clone(), self.body.clone());
        if api.lose_next_response {
            api.lose_next_response = false;
            CommitStatus::Unknown {
                reason: "response lost after server processed request".to_string(),
            }
        } else {
            CommitStatus::Confirmed(self.id.clone())
        }
    }

    fn reconcile(
        &self,
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

    fn rollback(&self, api: &mut FakeApi, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
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
    let mut api = FakeApi {
        lose_next_response: true,
        ..FakeApi::default()
    };
    let action = CreateResource {
        id: "r-42".to_string(),
        body: "payload".to_string(),
    };
    let result = Kernel::default().execute(action, &mut api)?;
    println!(
        "outcome={:?} reconciliation={:?}",
        result.outcome, result.record.reconciliation
    );
    Ok(())
}
