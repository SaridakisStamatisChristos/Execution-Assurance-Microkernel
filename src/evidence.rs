use crate::{
    durable_log::open_durable_jsonl, error::FailureClass, state::StateTransition, ExecutionState,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
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
        Self {
            name: name.into(),
            passed: true,
            reason: reason.into(),
        }
    }

    pub fn fail(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed: false,
            reason: reason.into(),
        }
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
    /// `true` means verification could not establish either success or
    /// failure because the observation path itself failed. This is not an
    /// explicit failed postcondition and must not trigger compensation.
    #[serde(default)]
    pub indeterminate: bool,
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
pub struct RecoveryRecord {
    pub resumed: bool,
    pub from_state: ExecutionState,
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
    VerificationRequired,
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
    #[serde(default)]
    pub recovery: Option<RecoveryRecord>,
    pub failure: Option<FailureRecord>,
    pub outcome: ExecutionOutcome,
    pub record_hash: Option<String>,
}

impl ExecutionRecord {
    pub fn seal(mut self) -> Result<Self, serde_json::Error> {
        self.record_hash = None;
        self.record_hash = Some(self.compute_hash()?);
        Ok(self)
    }

    pub fn verify_hash(&self) -> Result<bool, serde_json::Error> {
        let Some(expected) = self.record_hash.as_ref() else {
            return Ok(false);
        };
        Ok(&self.compute_hash()? == expected)
    }

    fn compute_hash(&self) -> Result<String, serde_json::Error> {
        let mut unsigned = self.clone();
        unsigned.record_hash = None;
        let canonical = serde_json::to_vec(&unsigned)?;
        let mut hasher = Sha256::new();
        hasher.update(canonical);
        Ok(format!("{:x}", hasher.finalize()))
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
    fn find(&self, execution_id: &str) -> Result<Option<ExecutionRecord>, String>;
}

#[derive(Debug, Default)]
pub struct InMemoryEvidenceStore {
    records: Mutex<Vec<ExecutionRecord>>,
}

impl InMemoryEvidenceStore {
    pub fn records(&self) -> Result<Vec<ExecutionRecord>, String> {
        self.records
            .lock()
            .map(|records| records.clone())
            .map_err(|_| "evidence lock poisoned".to_string())
    }
}

impl EvidenceStore for InMemoryEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        self.records
            .lock()
            .map_err(|_| "evidence lock poisoned".to_string())?
            .push(record.clone());
        Ok(())
    }

    fn find(&self, execution_id: &str) -> Result<Option<ExecutionRecord>, String> {
        Ok(self
            .records
            .lock()
            .map_err(|_| "evidence lock poisoned".to_string())?
            .iter()
            .rev()
            .find(|record| record.execution_id == execution_id)
            .cloned())
    }
}

#[derive(Debug)]
pub struct JsonlEvidenceStore {
    path: PathBuf,
    file: Mutex<File>,
}

impl JsonlEvidenceStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        let file = open_durable_jsonl(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }
}

impl EvidenceStore for JsonlEvidenceStore {
    fn persist(&self, record: &ExecutionRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| "evidence file lock poisoned".to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())
    }

    fn find(&self, execution_id: &str) -> Result<Option<ExecutionRecord>, String> {
        let file = File::open(&self.path).map_err(|error| error.to_string())?;
        let mut found = None;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|error| error.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let record: ExecutionRecord =
                serde_json::from_str(&line).map_err(|error| error.to_string())?;
            if record.execution_id == execution_id {
                found = Some(record);
            }
        }
        Ok(found)
    }
}

pub trait EvidenceRedactor: Send + Sync {
    fn redact(&self, record: &mut ExecutionRecord);
}

#[derive(Debug, Default)]
pub struct ConservativeRedactor {
    secrets: Vec<String>,
}

impl ConservativeRedactor {
    pub fn with_secrets<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            secrets: secrets.into_iter().map(Into::into).collect(),
        }
    }

    fn redact_text(&self, value: &str) -> String {
        let mut redacted = value.to_string();
        for secret in &self.secrets {
            if !secret.is_empty() {
                redacted = redacted.replace(secret, "[REDACTED]");
            }
        }
        for marker in [
            "password=",
            "token=",
            "secret=",
            "api_key=",
            "authorization=",
        ] {
            let mut cursor = 0;
            loop {
                if cursor >= redacted.len() {
                    break;
                }
                let lower_tail = redacted[cursor..].to_ascii_lowercase();
                let Some(relative_start) = lower_tail.find(marker) else {
                    break;
                };
                let start = cursor + relative_start;
                let value_start = start + marker.len();
                let end = redacted[value_start..]
                    .find(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '&'))
                    .map_or(redacted.len(), |offset| value_start + offset);
                if value_start == end {
                    cursor = value_start;
                    continue;
                }
                redacted.replace_range(value_start..end, "[REDACTED]");
                cursor = value_start + "[REDACTED]".len();
            }
        }
        redacted
    }

    fn redact_checks(&self, checks: &mut [CheckRecord]) {
        for check in checks {
            check.name = self.redact_text(&check.name);
            check.reason = self.redact_text(&check.reason);
        }
    }
}

impl EvidenceRedactor for ConservativeRedactor {
    fn redact(&self, record: &mut ExecutionRecord) {
        record.action_id = self.redact_text(&record.action_id);
        record.action_type = self.redact_text(&record.action_type);
        record.idempotency_key = record
            .idempotency_key
            .take()
            .map(|value| self.redact_text(&value));
        self.redact_checks(&mut record.validation);
        self.redact_checks(&mut record.preconditions);
        self.redact_checks(&mut record.invariants_before);
        self.redact_checks(&mut record.invariants_after);
        self.redact_checks(&mut record.invariants_after_rollback);
        record.commit.detail = self.redact_text(&record.commit.detail);
        if let Some(reconciliation) = record.reconciliation.as_mut() {
            reconciliation.detail = self.redact_text(&reconciliation.detail);
        }
        if let Some(verification) = record.verification.as_mut() {
            verification.detail = self.redact_text(&verification.detail);
            self.redact_checks(&mut verification.checks);
        }
        if let Some(rollback) = record.rollback.as_mut() {
            rollback.detail = self.redact_text(&rollback.detail);
            self.redact_checks(&mut rollback.checks);
        }
        if let Some(recovery) = record.recovery.as_mut() {
            recovery.detail = self.redact_text(&recovery.detail);
        }
        if let Some(failure) = record.failure.as_mut() {
            failure.message = self.redact_text(&failure.message);
        }
    }
}
