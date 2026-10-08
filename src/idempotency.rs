use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(pub String);

impl From<&str> for IdempotencyKey {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionClaimOutcome {
    Claimed,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Duplicate { original_execution_id: String },
}

pub trait IdempotencyStore: Send + Sync {
    fn claim_execution_id(&self, execution_id: &str) -> Result<ExecutionClaimOutcome, String>;
    fn claim(&self, key: &IdempotencyKey, execution_id: &str) -> Result<ClaimOutcome, String>;
}

#[derive(Debug, Default)]
pub struct InMemoryIdempotencyStore {
    execution_ids: Mutex<HashSet<String>>,
    claims: Mutex<HashMap<IdempotencyKey, String>>,
}

impl IdempotencyStore for InMemoryIdempotencyStore {
    fn claim_execution_id(&self, execution_id: &str) -> Result<ExecutionClaimOutcome, String> {
        let mut ids = self
            .execution_ids
            .lock()
            .map_err(|_| "execution-id lock poisoned".to_string())?;
        if !ids.insert(execution_id.to_string()) {
            return Ok(ExecutionClaimOutcome::Duplicate);
        }
        Ok(ExecutionClaimOutcome::Claimed)
    }

    fn claim(&self, key: &IdempotencyKey, execution_id: &str) -> Result<ClaimOutcome, String> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| "idempotency lock poisoned".to_string())?;
        if let Some(existing) = claims.get(key) {
            return Ok(ClaimOutcome::Duplicate {
                original_execution_id: existing.clone(),
            });
        }
        claims.insert(key.clone(), execution_id.to_string());
        Ok(ClaimOutcome::Claimed)
    }
}

/// Durable local identity/idempotency registry using one atomically-created claim
/// file per execution ID or idempotency key.
#[derive(Debug)]
pub struct FileIdempotencyStore {
    root: PathBuf,
}

impl FileIdempotencyStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn digest(namespace: &str, value: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(namespace.as_bytes());
        hasher.update([0]);
        hasher.update(value.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn claim_path(&self, namespace: &str, value: &str) -> PathBuf {
        self.root.join(format!(
            "{namespace}-{}.claim",
            Self::digest(namespace, value)
        ))
    }

    fn create_claim(&self, path: &Path, value: &str) -> Result<bool, String> {
        let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        file.write_all(value.as_bytes())
            .map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            std::fs::File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| error.to_string())?;
        }
        Ok(true)
    }

    fn read_claim(path: &Path) -> Result<String, String> {
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut value = String::new();
        file.read_to_string(&mut value)
            .map_err(|error| error.to_string())?;
        Ok(value)
    }
}

impl IdempotencyStore for FileIdempotencyStore {
    fn claim_execution_id(&self, execution_id: &str) -> Result<ExecutionClaimOutcome, String> {
        let path = self.claim_path("execution", execution_id);
        if self.create_claim(&path, execution_id)? {
            Ok(ExecutionClaimOutcome::Claimed)
        } else {
            Ok(ExecutionClaimOutcome::Duplicate)
        }
    }

    fn claim(&self, key: &IdempotencyKey, execution_id: &str) -> Result<ClaimOutcome, String> {
        let path = self.claim_path("idempotency", &key.0);
        if self.create_claim(&path, execution_id)? {
            return Ok(ClaimOutcome::Claimed);
        }
        Ok(ClaimOutcome::Duplicate {
            original_execution_id: Self::read_claim(&path)?,
        })
    }
}
