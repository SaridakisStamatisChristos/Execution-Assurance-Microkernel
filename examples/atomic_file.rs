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
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target has no parent directory",
            )
        })?;
        fs::File::open(parent)?.sync_all()
    }

    #[cfg(not(unix))]
    fn sync_parent(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn write_and_rename(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = path.with_extension("execution-microkernel.tmp");
        let mut options = fs::OpenOptions::new();
        use std::io::Write;
        let mut file = options.create(true).truncate(true).write(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(tmp, path)
    }

    fn commit_replace_with<F>(&self, sync_parent: F) -> CommitStatus<String, io::Error>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        if let Err(error) = Self::write_and_rename(&self.path, &self.replacement) {
            return CommitStatus::Failed(error);
        }

        let output = Self::hash(&self.replacement);
        match sync_parent(&self.path) {
            Ok(()) => CommitStatus::Confirmed(output),
            Err(error) => CommitStatus::Unknown {
                reason: format!(
                    "replacement is visible but parent-directory durability could not be established: {error}"
                ),
            },
        }
    }

    fn reconcile_with<F>(
        &self,
        sync_parent: F,
    ) -> Result<ReconciliationResult<String>, io::Error>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        match fs::read(&self.path) {
            Ok(bytes) if bytes == self.replacement => match sync_parent(&self.path) {
                Ok(()) => Ok(ReconciliationResult::Committed(Self::hash(&bytes))),
                Err(error) => Ok(ReconciliationResult::Unresolved {
                    reason: format!(
                        "replacement is visible but parent-directory durability remains unestablished: {error}"
                    ),
                }),
            },
            Ok(_) => Ok(ReconciliationResult::Unresolved {
                reason: "target exists with content different from proposed replacement".to_string(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ReconciliationResult::NotCommitted)
            }
            Err(error) => Err(error),
        }
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
            Some(bytes) => Self::write_and_rename(&self.path, bytes),
            None => match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        };
        match restored {
            Ok(()) => RollbackStatus::Succeeded,
            Err(error) => RollbackStatus::Failed(error),
        }
    }

    fn verify_rollback_with<F>(
        &self,
        snapshot: &Option<Vec<u8>>,
        sync_parent: F,
    ) -> Result<Vec<CheckRecord>, io::Error>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        let passed = match snapshot {
            Some(bytes) => match fs::read(&self.path) {
                Ok(current) => current == *bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            },
            None => !self.path.exists(),
        };

        if !passed {
            return Ok(vec![CheckRecord::fail(
                "rollback_state",
                "pre-execution file state not restored",
            )]);
        }

        sync_parent(&self.path)?;
        Ok(vec![CheckRecord::pass(
            "rollback_state",
            "pre-execution file state restored and parent-directory durability established",
        )])
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
        self.commit_replace_with(Self::sync_parent)
    }

    fn reconcile(
        &self,
        _permit: &EffectPermit,
        _ctx: &mut (),
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        self.reconcile_with(Self::sync_parent)
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
        self.verify_rollback_with(snapshot, Self::sync_parent)
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

    #[test]
    fn parent_sync_failure_after_rename_is_unknown_not_failed() {
        let root = std::env::temp_dir().join(format!("eamk-file-sync-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.txt");
        fs::write(&path, b"before").unwrap();
        let action = AtomicReplace {
            path: path.clone(),
            replacement: b"ours".to_vec(),
        };

        let status = action.commit_replace_with(|_| Err(io::Error::other("sync failed")));
        assert!(matches!(status, CommitStatus::Unknown { .. }));
        assert_eq!(fs::read(&path).unwrap(), b"ours");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reconciliation_does_not_confirm_commit_until_parent_durability_is_established() {
        let root = std::env::temp_dir().join(format!("eamk-file-reconcile-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.txt");
        fs::write(&path, b"before").unwrap();
        let action = AtomicReplace {
            path: path.clone(),
            replacement: b"ours".to_vec(),
        };

        let status = action.commit_replace_with(|_| Err(io::Error::other("sync failed")));
        assert!(matches!(status, CommitStatus::Unknown { .. }));

        let unresolved = action
            .reconcile_with(|_| Err(io::Error::other("sync still failed")))
            .unwrap();
        assert!(matches!(
            unresolved,
            ReconciliationResult::Unresolved { .. }
        ));

        let resolved = action.reconcile_with(|_| Ok(())).unwrap();
        assert!(matches!(resolved, ReconciliationResult::Committed(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_durability_uncertainty_is_left_for_verified_recovery() {
        let root = std::env::temp_dir().join(format!("eamk-file-rollback-sync-{}", Uuid::new_v4()));
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

        let error = action
            .verify_rollback_with(&snapshot, |_| Err(io::Error::other("sync failed")))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);

        let checks = action.verify_rollback_with(&snapshot, |_| Ok(())).unwrap();
        assert!(checks.iter().all(|check| check.passed));

        fs::remove_dir_all(root).unwrap();
    }
}
