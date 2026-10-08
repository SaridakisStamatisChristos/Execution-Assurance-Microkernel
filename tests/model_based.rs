mod common;

use common::{basic_kernel, kernel_with, TestAction, World};
use execution_assurance_microkernel::{
    EvidenceStore, ExecutionOutcome, FaultPoint, IdempotencyStore, InMemoryEvidenceStore,
    InMemoryIdempotencyStore, InMemoryJournal, Journal, RecoveryManager, RecoveryPlan,
    ScriptedFaultInjector,
};
use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #[test]
    fn success_iff_commit_and_verification_survive(
        delta in 0i32..100,
        allowed in any::<bool>(),
        verify_ok in any::<bool>(),
        rollback_ok in any::<bool>(),
    ) {
        let mut world = World::default();
        let action = TestAction {
            delta,
            allowed,
            verify_ok,
            rollback_ok,
            ..TestAction::default()
        };
        let result = basic_kernel().execute(action, &mut world).unwrap();

        if !allowed {
            prop_assert_eq!(result.outcome, ExecutionOutcome::Rejected);
            prop_assert_eq!(world.commits, 0);
            prop_assert_eq!(world.rollbacks, 0);
        } else if verify_ok {
            prop_assert_eq!(result.outcome, ExecutionOutcome::Success);
            prop_assert_eq!(world.commits, 1);
            prop_assert_eq!(world.value, delta);
            prop_assert!(result.record.verification.as_ref().unwrap().passed);
        } else if rollback_ok {
            prop_assert_eq!(result.outcome, ExecutionOutcome::RolledBack);
            prop_assert_eq!(world.commits, 1);
            prop_assert_eq!(world.rollbacks, 1);
            prop_assert_eq!(world.value, 0);
            prop_assert!(result.record.rollback.as_ref().unwrap().verified);
        } else {
            prop_assert_eq!(result.outcome, ExecutionOutcome::RollbackFailed);
            prop_assert_eq!(world.commits, 1);
            prop_assert_eq!(world.rollbacks, 1);
        }

        if result.outcome == ExecutionOutcome::Success {
            prop_assert!(result.record.verification.as_ref().unwrap().passed);
            prop_assert!(matches!(
                result.record.commit.disposition,
                execution_assurance_microkernel::CommitDisposition::Confirmed
                    | execution_assurance_microkernel::CommitDisposition::ReconciledCommitted
            ));
        }
    }

    #[test]
    fn any_defined_fault_point_has_a_recoverable_non_success_head(index in 0usize..FaultPoint::ALL.len()) {
        let point = FaultPoint::ALL[index];
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
        prop_assert!(error.to_string().contains("fault injected"));

        let directives = RecoveryManager::scan(journal.as_ref()).unwrap();
        prop_assert_eq!(directives.len(), 1);
        prop_assert_ne!(directives[0].plan, RecoveryPlan::None);
    }
}
