use std::{collections::HashMap, sync::Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(pub String);

impl From<&str> for IdempotencyKey {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Duplicate { original_execution_id: String },
}

pub trait IdempotencyStore: Send + Sync {
    fn claim(&self, key: &IdempotencyKey, execution_id: &str) -> Result<ClaimOutcome, String>;
}

#[derive(Debug, Default)]
pub struct InMemoryIdempotencyStore {
    claims: Mutex<HashMap<IdempotencyKey, String>>,
}

impl IdempotencyStore for InMemoryIdempotencyStore {
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
