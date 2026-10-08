use execution_assurance_microkernel::{
    ExecutionState, FileJournal, Journal, JournalEntry, RecoveryManager, RecoveryPlan,
};
use std::fs;
use uuid::Uuid;

#[test]
fn file_journal_survives_reopen_and_preserves_order() {
    let path = std::env::temp_dir().join(format!("eamk-journal-{}.jsonl", Uuid::new_v4()));

    {
        let journal = FileJournal::open(&path).unwrap();
        journal
            .append(JournalEntry::state("exec-1", ExecutionState::Prepared, 10))
            .unwrap();
        journal
            .append(JournalEntry::state("exec-1", ExecutionState::Committed, 11))
            .unwrap();
    }

    let reopened = FileJournal::open(&path).unwrap();
    let entries = reopened.entries().unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].state, ExecutionState::Prepared);
    assert_eq!(entries[1].state, ExecutionState::Committed);
    assert_eq!(
        RecoveryManager::scan(&reopened).unwrap()[0].plan,
        RecoveryPlan::VerifyOrRollback
    );

    fs::remove_file(path).unwrap();
}

#[test]
fn recovery_uses_the_latest_durable_state_per_execution() {
    let path = std::env::temp_dir().join(format!("eamk-recovery-{}.jsonl", Uuid::new_v4()));
    let journal = FileJournal::open(&path).unwrap();

    for (execution_id, state, at_ms) in [
        ("a", ExecutionState::Prepared, 1),
        ("b", ExecutionState::Prepared, 2),
        ("a", ExecutionState::Committed, 3),
        ("b", ExecutionState::RollbackPending, 4),
    ] {
        journal
            .append(JournalEntry::state(execution_id, state, at_ms))
            .unwrap();
    }

    let directives = RecoveryManager::scan(&journal).unwrap();
    assert_eq!(directives.len(), 2);
    let a = directives
        .iter()
        .find(|directive| directive.execution_id == "a")
        .unwrap();
    let b = directives
        .iter()
        .find(|directive| directive.execution_id == "b")
        .unwrap();
    assert_eq!(a.plan, RecoveryPlan::VerifyOrRollback);
    assert_eq!(b.plan, RecoveryPlan::CompleteRollback);

    drop(journal);
    fs::remove_file(path).unwrap();
}
