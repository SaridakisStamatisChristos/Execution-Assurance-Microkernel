mod common;

use common::{basic_kernel, TestAction, World};
use execution_assurance_microkernel::{
    CheckRecord, ConservativeRedactor, EvidenceRedactor, EvidenceStore, JsonlEvidenceStore,
};
use std::fs;
use uuid::Uuid;

#[test]
fn successful_execution_emits_a_verifiable_seal() {
    let mut world = World::default();
    let result = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap();

    assert!(result.record.record_hash.is_some());
    assert!(result.record.verify_hash().unwrap());
}

#[test]
fn mutating_a_sealed_record_invalidates_the_hash() {
    let mut world = World::default();
    let mut record = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap()
        .record;

    assert!(record.verify_hash().unwrap());
    record.action_id.push_str("-tampered");
    assert!(!record.verify_hash().unwrap());
}

#[test]
fn an_unsealed_record_does_not_verify() {
    let mut world = World::default();
    let mut record = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap()
        .record;

    record.record_hash = None;
    assert!(!record.verify_hash().unwrap());
}

#[test]
fn jsonl_evidence_round_trips_as_machine_inspectable_data() {
    let mut world = World::default();
    let record = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap()
        .record;

    let path = std::env::temp_dir().join(format!("eamk-evidence-{}.jsonl", Uuid::new_v4()));
    let store = JsonlEvidenceStore::open(&path).unwrap();
    store.persist(&record).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    let persisted: execution_assurance_microkernel::ExecutionRecord =
        serde_json::from_str(text.trim()).unwrap();

    assert_eq!(persisted.execution_id, record.execution_id);
    assert_eq!(persisted.record_hash, record.record_hash);
    assert!(persisted.verify_hash().unwrap());
    assert!(store.find(&record.execution_id).unwrap().is_some());

    fs::remove_file(path).unwrap();
}

#[test]
fn raw_secrets_are_redacted_before_durable_evidence() {
    let mut world = World::default();
    let mut record = basic_kernel()
        .execute(TestAction::default(), &mut world)
        .unwrap()
        .record;

    record.action_id = "token=tok-123 registered-secret".to_string();
    record.commit.detail = "authorization=Bearer-456 password=hunter2".to_string();
    record.preconditions.push(CheckRecord::fail(
        "secret-check",
        "api_key=key-789 secret=inline-secret registered-secret",
    ));
    record.record_hash = None;

    let redactor = ConservativeRedactor::with_secrets(["registered-secret"]);
    redactor.redact(&mut record);
    let record = record.seal().unwrap();

    let path = std::env::temp_dir().join(format!("eamk-redacted-{}.jsonl", Uuid::new_v4()));
    let store = JsonlEvidenceStore::open(&path).unwrap();
    store.persist(&record).unwrap();
    let text = fs::read_to_string(&path).unwrap();

    for secret in [
        "tok-123",
        "Bearer-456",
        "hunter2",
        "key-789",
        "inline-secret",
        "registered-secret",
    ] {
        assert!(!text.contains(secret), "secret leaked into evidence: {secret}");
    }
    assert!(text.contains("[REDACTED]"));
    assert!(record.verify_hash().unwrap());

    fs::remove_file(path).unwrap();
}
