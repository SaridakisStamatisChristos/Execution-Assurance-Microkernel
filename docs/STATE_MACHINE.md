# State Machine

The state machine is explicit and closed: only transitions accepted by `ExecutionState::can_transition_to` are legal.

```text
Created
  |
  v
Proposed ---- validation/precondition/idempotency failure ----> Rejected
  |
  v
Validated ---- snapshot/invariant failure --------------------> Aborted
  |
  v
Prepared ---- known commit failure ---------------------------> Failed
  |  \
  |   +---- uncertain effect ----> ReconciliationRequired ----> Aborted
  |                                      |                       (proved absent)
  |                                      |
  |                                      +--------------------> Committed
  |                                                              (proved present)
  v
Committed ---- recovery may require readback ----> ReconciliationRequired
  |                                                   |
  |                                                   +--------> Committed
  |
  +---- non-compensable verification failure -----------------> Failed
  |
  +---- compensable verification/invariant failure --> RollbackPending --> RolledBack
  |                                                                  \
  |                                                                   +--> Failed
  v
Verified
  |
  v
Finalized
```

`ReconciliationRequired` is deliberately **nonterminal**. It represents an unresolved execution whose external effect cannot yet be classified safely.

## Normal path

```text
Created -> Proposed -> Validated -> Prepared -> Committed -> Verified -> Finalized
```

The kernel cannot reach `Prepared` until validation, preconditions, snapshot capture, and pre-commit invariants succeed. It cannot reach `Verified` until an effect is established and postconditions independently pass. It cannot durably write `Finalized` until the sealed evidence record has been persisted.

## Failure paths

- `Proposed -> Rejected`: validation, precondition, identity, or idempotency refusal before effect.
- `Validated -> Aborted`: snapshot or pre-commit invariant failure.
- `Prepared -> Failed`: commit is known not to have succeeded.
- `Prepared -> ReconciliationRequired`: effect outcome is unknown.
- `Committed -> RollbackPending`: verification or post-commit invariant failed and the action is compensable.
- `Committed -> Failed`: verification/invariant failed and the action explicitly has no safe compensation path.
- `RollbackPending -> RolledBack`: compensation and rollback verification both succeed.
- `RollbackPending -> Failed`: compensation or rollback verification fails.

## Durable recovery interpretation

| Last durable state | Recovery plan | Kernel behavior |
|---|---|---|
| `Created`, `Proposed`, `Validated` | `AbortBeforeCommit` | terminate without invoking `commit` |
| `Prepared` | `ReconcileBeforeRetry` | use action readback; never blind retry |
| `ReconciliationRequired` | `ReconcileBeforeRetry` | retry reconciliation only, not commit |
| `Committed` | `VerifyOrRollback` | re-establish output through reconciliation, then verify or compensate |
| `RollbackPending` | `CompleteRollback` | first verify whether compensation already completed; otherwise resume it |
| `Verified` | `FinalizeVerified` | persist/finalize without repeating the effect |
| `Finalized`, `Rejected`, `Aborted`, `RolledBack`, `Failed` | `None` | settled |

`Prepared` is intentionally conservative. A process may crash after the remote side effect but before the local `Committed` append. The durable recovery envelope stored with `Prepared` carries the action identity, idempotency identity, start time, compensation policy, and serialized rollback snapshot required by later recovery.

## Terminal-state ordering

For K5, terminal durability is ordered as:

```text
construct terminal transition in memory
-> redact and seal ExecutionRecord
-> persist evidence
-> append terminal journal state + fsync
-> return ExecutionResult
```

Therefore a durable terminal journal state implies that the corresponding evidence record already exists. If evidence persistence fails, the journal remains at a recoverable nonterminal state such as `Verified` or `RollbackPending`.
