use execution_assurance_microkernel::{
    Action, CheckRecord, CommitStatus, EffectPermit, IdempotencyKey, Kernel, Predicate,
    ReconciliationResult,
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
    fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = path.with_extension("execution-microkernel.tmp");
        let mut options = fs::OpenOptions::new();
        use std::io::Write;
        let mut file = options.create(true).truncate(true).write(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(tmp, path)
    }

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
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
        match snapshot {
            Some(bytes) => Self::replace(&self.path, bytes),
            None => match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        }
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
