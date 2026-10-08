mod common;

use common::{kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, FaultPoint, IdempotencyStore, InMemoryEvidenceStore, InMemoryIdempotencyStore,
    InMemoryJournal, Journal, RecoveryManager, RecoveryPlan, ScriptedFaultInjector,
};
use std::sync::Arc;

#[test]
fn every_fault_point_is_deterministically_reproducible() {
    for point in FaultPoint::ALL {
        let journal = Arc::new(InMemoryJournal::default());
        let journal_trait: Arc<dyn Journal> = journal.clone();
        let evidence: Arc<dyn EvidenceStore> = Arc::new(InMemoryEvidenceStore::default());
        let ids: Arc<dyn IdempotencyStore> = Arc::new(InMemoryIdempotencyStore::default());
        let kernel = kernel_with(
            journal_trait,
            evidence,
            ids,
            Arc::new(ScriptedFaultInjector::with_fault(point)),
        );
        let mut world = World::default();
        let action = if matches!(point, FaultPoint::DuringRollback | FaultPoint::AfterRollback) {
            TestAction { verify_ok: false, ..TestAction::default() }
        } else {
            TestAction::default()
        };

        let error = kernel.execute(action, &mut world).unwrap_err();
        assert!(error.to_string().contains("fault injected"));
        let directives = RecoveryManager::scan(journal.as_ref()).unwrap();

        match point {
            FaultPoint::BeforeSnapshot | FaultPoint::AfterSnapshot => {
                assert_eq!(directives[0].plan, RecoveryPlan::AbortBeforeCommit);
            }
            FaultPoint::BeforeCommit | FaultPoint::DuringCommit | FaultPoint::AfterCommit => {
                assert_eq!(directives[0].plan, RecoveryPlan::ReconcileBeforeRetry);
            }
            FaultPoint::BeforeVerify => {
                assert_eq!(directives[0].plan, RecoveryPlan::VerifyOrRollback);
            }
            FaultPoint::DuringRollback | FaultPoint::AfterRollback => {
                assert_eq!(directives[0].plan, RecoveryPlan::CompleteRollback);
            }
        }
    }
}
