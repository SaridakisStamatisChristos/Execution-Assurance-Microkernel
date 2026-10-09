use execution_assurance_microkernel::{
    Action, CheckRecord, CommitStatus, EffectPermit, IdempotencyKey, Kernel, Predicate,
    ReconciliationResult, RollbackStatus,
};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
struct ParentExists(PathBuf);

impl Predicate<()> for ParentExists {
    fn evaluate(&self, _ctx: &()) -> CheckRecord {
        match self.0.parent() {
            Some(parent) if parent.is_dir() => {
                CheckRecord::pass("parent_exists", parent.display().to_string())
            }
            _ => CheckRecord::fail("parent_exists", "target parent is missing"),
        }
    }
}

#[derive(Debug)]
struct AtomicReplace {
    path: PathBuf,
    replacement: Vec<u8>,
}

impl AtomicReplace {
    #[cfg(unix)]
    fn sync_parent(path: &Path) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "target has no parent directory")
        })?;
        fs::File::open(parent)?.sync_all()
    }

    #[cfg(not(unix))]
    fn sync_parent(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = path.with_extension("execution-microkernel.tmp");
        let mut options = fs::OpenOptions::new();
        use std::io::Write;
        let mut file = options.create(true).truncate(true).write(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
        Self::sync_parent(path)
    }

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn rollback_if_owned(&self, snapshot: &Option<Vec<u8>>) -> RollbackStatus<io::Error> {
        match fs::read(&self.path) {
            Ok(current) if current == self.replacement => {}
            Ok(_) => {
                return RollbackStatus::Conflict {
                    reason:
                        "target changed after commit; refusing to overwrite newer external state"
                            .to_string(),
                };
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return RollbackStatus::Conflict {
                    reason: "target disappeared after commit; compensation ownership is lost"
                        .to_string(),
                };
            }
            Err(error) => return RollbackStatus::Failed(error),
        }

        let restored = match snapshot {
            Some(bytes) => Self::replace(&self.path, bytes),
            None => match fs::remove_file(&self.path) {
                Ok(()) => Self::sync_parent(&self.path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        };
        match restored {
            Ok(()) => RollbackStatus::Succeeded,
            Err(error) => RollbackStatus::Failed(error),
        }
    }
}

impl Action for AtomicReplace {
    type Context = ();
    type Output = String;
    type Error = io::Error;
    type Snapshot = Option<Vec<u8>>;

    fn action_id(&self) -> String {
        format!("replace:{}", self.path.display())
    }

    fn action_type(&self) -> &'static str {
        "atomic_file_replace"
    }

    fn validate(&self, _ctx: &()) -> Result<(), Self::Error> {
        if self.replacement.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "replacement must not be empty",
            ));
        }
        Ok(())
    }

    fn preconditions(&self) -> Vec<Box<dyn Predicate<()> + '_>> {
        vec![Box::new(ParentExists(self.path.clone()))]
    }

    fn snapshot(&self, _ctx: &()) -> Result<Self::Snapshot, Self::Error> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn commit(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut (),
    ) -> CommitStatus<Self::Output, Self::Error> {
        match Self::replace(&self.path, &self.replacement) {
            Ok(()) => CommitStatus::Confirmed(Self::hash(&self.replacement)),
            Err(error) => CommitStatus::Failed(error),
        }
    }

    fn reconcile(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut (),
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        match fs::read(&self.path) {
            Ok(bytes) if bytes == self.replacement => {
                Ok(ReconciliationResult::Committed(Self::hash(&bytes)))
            }
            Ok(_) => Ok(ReconciliationResult::Unresolved {
                reason: "target exists with content different from proposed replacement"
                    .to_string(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ReconciliationResult::NotCommitted)
            }
            Err(error) => Err(error),
        }
    }

    fn verify(&self, _ctx: &(), output: &Self::Output) -> Result<Vec<CheckRecord>, Self::Error> {
        let persisted = fs::read(&self.path)?;
        let hash = Self::hash(&persisted);
        Ok(vec![if &hash == output {
            CheckRecord::pass("content_hash", hash)
        } else {
            CheckRecord::fail("content_hash", format!("expected {output}, got {hash}"))
        }])
    }

    fn rollback(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut (),
        snapshot: &Self::Snapshot,
    ) -> Result<(), Self::Error> {
        match self.rollback_if_owned(snapshot) {
            RollbackStatus::Succeeded => Ok(()),
            RollbackStatus::Failed(error) => Err(error),
            RollbackStatus::Conflict { reason } => Err(io::Error::other(reason)),
        }
    }

    fn rollback_status(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut (),
        snapshot: &Self::Snapshot,
    ) -> RollbackStatus<Self::Error> {
        self.rollback_if_owned(snapshot)
    }

    fn verify_rollback(
        &self,
        _ctx: &(),
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        let passed = match snapshot {
            Some(bytes) => fs::read(&self.path)? == *bytes,
            None => !self.path.exists(),
        };
        Ok(vec![if passed {
            CheckRecord::pass("rollback_state", "pre-execution file state restored")
        } else {
            CheckRecord::fail("rollback_state", "pre-execution file state not restored")
        }])
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("eamk-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let target = root.join("config.json");
    fs::write(&target, br#"{"version":1}"#)?;

    let action = AtomicReplace {
        path: target.clone(),
        replacement: br#"{"version":2}"#.to_vec(),
    };
    let result =
        Kernel::default().execute_idempotent(action, &mut (), IdempotencyKey::from("config-v2"))?;
    println!(
        "outcome={:?} hash={:?}",
        result.outcome, result.record.record_hash
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn compensation_refuses_to_overwrite_external_file_change() {
        let root = std::env::temp_dir().join(format!("eamk-file-conflict-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.txt");
        fs::write(&path, b"third-party").unwrap();
        let action = AtomicReplace {
            path: path.clone(),
            replacement: b"ours".to_vec(),
        };
        let snapshot = Some(b"before".to_vec());

        let status = action.rollback_if_owned(&snapshot);
        assert!(matches!(status, RollbackStatus::Conflict { .. }));
        assert_eq!(fs::read(&path).unwrap(), b"third-party");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compensation_restores_snapshot_when_committed_content_is_still_owned() {
        let root = std::env::temp_dir().join(format!("eamk-file-owned-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.txt");
        fs::write(&path, b"ours").unwrap();
        let action = AtomicReplace {
            path: path.clone(),
            replacement: b"ours".to_vec(),
        };
        let snapshot = Some(b"before".to_vec());

        let status = action.rollback_if_owned(&snapshot);
        assert!(matches!(status, RollbackStatus::Succeeded));
        assert_eq!(fs::read(&path).unwrap(), b"before");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_committed_file_completes_compensation() {
        let root = std::env::temp_dir().join(format!("eamk-file-remove-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.txt");
        fs::write(&path, b"ours").unwrap();
        let action = AtomicReplace {
            path: path.clone(),
            replacement: b"ours".to_vec(),
        };

        let status = action.rollback_if_owned(&None);
        assert!(matches!(status, RollbackStatus::Succeeded));
        assert!(!path.exists());

        fs::remove_dir_all(root).unwrap();
    }
}
