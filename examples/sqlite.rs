use execution_assurance_microkernel::{
    Action, CheckRecord, CommitStatus, EffectPermit, Kernel, Predicate, ReconciliationResult,
    RollbackStatus,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
struct RowVersion {
    account_id: i64,
    expected_version: i64,
}

impl Predicate<Connection> for RowVersion {
    fn evaluate(&self, conn: &Connection) -> CheckRecord {
        let result: rusqlite::Result<i64> = conn.query_row(
            "SELECT version FROM account WHERE id = ?1",
            [self.account_id],
            |row| row.get(0),
        );
        match result {
            Ok(version) if version == self.expected_version => {
                CheckRecord::pass("row_version", format!("version={version}"))
            }
            Ok(version) => CheckRecord::fail(
                "row_version",
                format!("expected {}, got {version}", self.expected_version),
            ),
            Err(error) => CheckRecord::fail("row_version", error.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountSnapshot {
    balance: i64,
    version: i64,
}

#[derive(Debug)]
struct UpdateAccount {
    account_id: i64,
    expected_version: i64,
    new_balance: i64,
}

impl UpdateAccount {
    fn rollback_if_owned(
        &self,
        conn: &mut Connection,
        snapshot: &AccountSnapshot,
    ) -> RollbackStatus<rusqlite::Error> {
        let changed = conn.execute(
            "UPDATE account SET balance = ?1, version = ?2 WHERE id = ?3 AND balance = ?4 AND version = ?5",
            params![
                snapshot.balance,
                snapshot.version,
                self.account_id,
                self.new_balance,
                self.expected_version + 1
            ],
        );
        match changed {
            Ok(1) => RollbackStatus::Succeeded,
            Ok(_) => RollbackStatus::Conflict {
                reason: "row changed after commit; refusing to overwrite newer database state"
                    .to_string(),
            },
            Err(error) => RollbackStatus::Failed(error),
        }
    }
}

impl Action for UpdateAccount {
    type Context = Connection;
    type Output = i64;
    type Error = rusqlite::Error;
    type Snapshot = AccountSnapshot;

    fn action_id(&self) -> String {
        format!("account:{}", self.account_id)
    }

    fn action_type(&self) -> &'static str {
        "sqlite_account_update"
    }

    fn validate(&self, _ctx: &Connection) -> Result<(), Self::Error> {
        Ok(())
    }

    fn preconditions(&self) -> Vec<Box<dyn Predicate<Connection> + '_>> {
        vec![Box::new(RowVersion {
            account_id: self.account_id,
            expected_version: self.expected_version,
        })]
    }

    fn snapshot(&self, conn: &Connection) -> Result<Self::Snapshot, Self::Error> {
        conn.query_row(
            "SELECT balance, version FROM account WHERE id = ?1",
            [self.account_id],
            |row| {
                Ok(AccountSnapshot {
                    balance: row.get(0)?,
                    version: row.get(1)?,
                })
            },
        )
    }

    fn commit(
        &self,
        _permit: &EffectPermit,
        conn: &mut Connection,
    ) -> CommitStatus<Self::Output, Self::Error> {
        let changed = conn.execute(
            "UPDATE account SET balance = ?1, version = version + 1 WHERE id = ?2 AND version = ?3",
            params![self.new_balance, self.account_id, self.expected_version],
        );
        match changed {
            Ok(1) => CommitStatus::Confirmed(self.expected_version + 1),
            Ok(_) => CommitStatus::Failed(rusqlite::Error::QueryReturnedNoRows),
            Err(error) => CommitStatus::Failed(error),
        }
    }

    fn reconcile(
        &self,
        _permit: &EffectPermit,
        conn: &mut Connection,
    ) -> Result<ReconciliationResult<Self::Output>, Self::Error> {
        let (balance, version): (i64, i64) = conn.query_row(
            "SELECT balance, version FROM account WHERE id = ?1",
            [self.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(
            if balance == self.new_balance && version == self.expected_version + 1 {
                ReconciliationResult::Committed(version)
            } else if version == self.expected_version {
                ReconciliationResult::NotCommitted
            } else {
                ReconciliationResult::Unresolved {
                    reason: format!(
                    "row is at unexpected balance/version {balance}/{version}; cannot infer commit"
                ),
                }
            },
        )
    }

    fn verify(
        &self,
        conn: &Connection,
        output: &Self::Output,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        let (balance, version): (i64, i64) = conn.query_row(
            "SELECT balance, version FROM account WHERE id = ?1",
            [self.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(vec![
            if balance == self.new_balance {
                CheckRecord::pass("balance", balance.to_string())
            } else {
                CheckRecord::fail("balance", balance.to_string())
            },
            if version == *output {
                CheckRecord::pass("version", version.to_string())
            } else {
                CheckRecord::fail("version", version.to_string())
            },
        ])
    }

    fn rollback(
        &self,
        _permit: &EffectPermit,
        conn: &mut Connection,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Self::Error> {
        match self.rollback_if_owned(conn, snapshot) {
            RollbackStatus::Succeeded => Ok(()),
            RollbackStatus::Failed(error) => Err(error),
            RollbackStatus::Conflict { .. } => Err(rusqlite::Error::ExecuteReturnedResults),
        }
    }

    fn rollback_status(
        &self,
        _permit: &EffectPermit,
        conn: &mut Connection,
        snapshot: &Self::Snapshot,
    ) -> RollbackStatus<Self::Error> {
        self.rollback_if_owned(conn, snapshot)
    }

    fn verify_rollback(
        &self,
        conn: &Connection,
        snapshot: &Self::Snapshot,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        let (balance, version): (i64, i64) = conn.query_row(
            "SELECT balance, version FROM account WHERE id = ?1",
            [self.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(vec![
            if balance == snapshot.balance && version == snapshot.version {
                CheckRecord::pass("rollback_state", "row restored")
            } else {
                CheckRecord::fail("rollback_state", "row differs from snapshot")
            },
        ])
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = Connection::open_in_memory()?;
    conn.execute_batch("CREATE TABLE account(id INTEGER PRIMARY KEY, balance INTEGER NOT NULL, version INTEGER NOT NULL); INSERT INTO account VALUES(1, 100, 7);")?;
    let action = UpdateAccount {
        account_id: 1,
        expected_version: 7,
        new_balance: 125,
    };
    let result = Kernel::default().execute(action, &mut conn)?;
    println!("outcome={:?}", result.outcome);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE account(id INTEGER PRIMARY KEY, balance INTEGER NOT NULL, version INTEGER NOT NULL); INSERT INTO account VALUES(1, 100, 7);")
            .unwrap();
        conn
    }

    #[test]
    fn compensation_refuses_to_overwrite_concurrent_row_update() {
        let mut conn = setup();
        conn.execute(
            "UPDATE account SET balance = 130, version = 9 WHERE id = 1",
            [],
        )
        .unwrap();
        let action = UpdateAccount {
            account_id: 1,
            expected_version: 7,
            new_balance: 125,
        };
        let snapshot = AccountSnapshot {
            balance: 100,
            version: 7,
        };

        let status = action.rollback_if_owned(&mut conn, &snapshot);
        assert!(matches!(status, RollbackStatus::Conflict { .. }));
        let row: (i64, i64) = conn
            .query_row(
                "SELECT balance, version FROM account WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (130, 9));
    }

    #[test]
    fn compensation_restores_snapshot_only_from_expected_committed_version() {
        let mut conn = setup();
        conn.execute(
            "UPDATE account SET balance = 125, version = 8 WHERE id = 1",
            [],
        )
        .unwrap();
        let action = UpdateAccount {
            account_id: 1,
            expected_version: 7,
            new_balance: 125,
        };
        let snapshot = AccountSnapshot {
            balance: 100,
            version: 7,
        };

        let status = action.rollback_if_owned(&mut conn, &snapshot);
        assert!(matches!(status, RollbackStatus::Succeeded));
        let row: (i64, i64) = conn
            .query_row(
                "SELECT balance, version FROM account WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (100, 7));
    }
}
