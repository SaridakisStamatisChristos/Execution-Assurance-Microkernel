use crate::{CompensationPolicy, ExecutionState};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEnvelope {
    pub action_id: String,
    pub action_type: String,
    pub idempotency_key: Option<String>,
    pub started_at_ms: u64,
    pub compensation_policy: CompensationPolicy,
    pub snapshot: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub execution_id: String,
    pub state: ExecutionState,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryEnvelope>,
}

impl JournalEntry {
    pub fn state(execution_id: impl Into<String>, state: ExecutionState, at_ms: u64) -> Self {
        Self {
            execution_id: execution_id.into(),
            state,
            at_ms,
            recovery: None,
        }
    }
}

pub trait Journal: Send + Sync {
    fn append(&self, entry: JournalEntry) -> Result<(), String>;
    fn entries(&self) -> Result<Vec<JournalEntry>, String>;
}

#[derive(Debug, Default)]
pub struct InMemoryJournal {
    entries: Mutex<Vec<JournalEntry>>,
}

impl Journal for InMemoryJournal {
    fn append(&self, entry: JournalEntry) -> Result<(), String> {
        self.entries
            .lock()
            .map_err(|_| "journal lock poisoned".to_string())?
            .push(entry);
        Ok(())
    }

    fn entries(&self) -> Result<Vec<JournalEntry>, String> {
        self.entries
            .lock()
            .map(|entries| entries.clone())
            .map_err(|_| "journal lock poisoned".to_string())
    }
}

#[derive(Debug)]
pub struct FileJournal {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }
}

impl Journal for FileJournal {
    fn append(&self, entry: JournalEntry) -> Result<(), String> {
        let bytes = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| "journal file lock poisoned".to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())
    }

    fn entries(&self) -> Result<Vec<JournalEntry>, String> {
        let file = File::open(&self.path).map_err(|error| error.to_string())?;
        BufReader::new(file)
            .lines()
            .filter(|line| line.as_ref().map_or(true, |line| !line.trim().is_empty()))
            .map(|line| {
                let line = line.map_err(|error| error.to_string())?;
                serde_json::from_str(&line).map_err(|error| error.to_string())
            })
            .collect()
    }
}
