mod common;

use common::{basic_kernel, TestAction, World};
use execution_assurance_microkernel::ExecutionOutcome;
use proptest::prelude::*;

proptest! {
    #[test]
    fn success_iff_commit_and_verification_survive(delta in 0i32..100, verify_ok in any::<bool>()) {
        let mut world = World::default();
        let action = TestAction { delta, verify_ok, ..TestAction::default() };
        let result = basic_kernel().execute(action, &mut world).unwrap();
        if verify_ok {
            prop_assert_eq!(result.outcome, ExecutionOutcome::Success);
            prop_assert_eq!(world.value, delta);
        } else {
            prop_assert_eq!(result.outcome, ExecutionOutcome::RolledBack);
            prop_assert_eq!(world.value, 0);
        }
        prop_assert_eq!(world.commits, 1);
    }
}
