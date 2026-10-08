use crate::{error::FailureClass, state::StateTransition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub name: String,
    pub passed: bool,
    pub reason: String,
}

impl CheckRecord {
    pub fn pass(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, reason: reason.into() }
    }

    pub fn fail(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { name: name.into(), passed: false, reason: reason.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitDisposition {
    NotAttempted,
    Confirmed,
    Failed,
    Unknown,
    ReconciledCommitted,
    ReconciledNotCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecord {
    pub disposition: CommitDisposition,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub passed: bool,
    pub checks: Vec<CheckRecord>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationRecord {
    pub attempted: bool,
    pub resolved: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackRecord {
    pub attempted: bool,
    pub succeeded: bool,
    pub verified: bool,
    pub checks: Vec<CheckRecord>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecord {
    pub class: FailureClass,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Success,
    Rejected,
    Aborted,
    CommitFailed,
    ReconciliationRequired,
    RolledBack,
    RollbackFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub schema_version: u32,
    pub execution_id: String,
    pub action_id: String,
    pub action_type: String,
    pub idempotency_key: Option<String>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub transitions: Vec<StateTransition>,
    pub validation: Vec<CheckRecord>,
    pub preconditions: Vec<CheckRecord>,
    pub invariants_before: Vec<CheckRecord>,
    pub invariants_after: Vec<CheckRecord>,
    pub invariants_after_rollback: Vec<CheckRecord>,
    pub commit: CommitRecord,
    pub reconciliation: Option<ReconciliationRecord>,
    pub verification: Option<VerificationRecord>,
    pub rollback: Option<RollbackRecord>,
    pub failure: Option<FailureRecord>,
    pub outcome: ExecutionOutcome,
    pub record_hash: Option<String>,
}

impl ExecutionRecord {
    pub fn seal(mut self) -> Result<Self, serde_json::Error> {
        self.record_hash = None;
        let canonical = serde_json::to_vec(&self)?;
        let mut hasher = Sha256::new();
        hasher.update(canonical);
        self.record_hash = Some(format!("{:x}", hasher.finalize()));
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionResult<T> {
    pub execution_id: String,
    pub outcome: ExecutionOutcome,
    pub output: Option<T>,
    pub record: ExecutionRecord,
}

pub trait EvidenceStore: Send + Sync {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct InMemoryEvidenceStore {
    records: Mutex<Vec<ExecutionRecord>>,
}

impl InMemoryEvidenceStore {
    pub fn records(&self) -> Result<Vec<ExecutionRecord>, String> {
        self.records.lock().map(|records| records.clone()).map_err(|_| "evidence lock poisoned".to_string())
    }
}

impl EvidenceStore for InMemoryEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        self.records.lock().map_err(|_| "evidence lock poisoned".to_string())?.push(record.clone());
        Ok(())
    }
}

#[derive(Debug)]
pub struct JsonlEvidenceStore {
    file: Mutex<File>,
}

impl JsonlEvidenceStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file: Mutex::new(file) })
    }
}

impl EvidenceStore for JsonlEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let mut file = self.file.lock().map_err(|_| "evidence file lock poisoned".to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())
    }
}
