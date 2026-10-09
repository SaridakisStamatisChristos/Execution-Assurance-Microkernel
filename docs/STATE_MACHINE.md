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
Committed ---- recovery/readback ----> ReconciliationRequired ----> Committed
  |
  +---- verification observer unavailable --------------------> [remain Committed]
  |                                                            outcome=VerificationRequired
  |
  +---- non-compensable explicit verification failure --------> Failed
  |
  +---- compensable explicit verification/invariant failure -> RollbackPending
  |                                                              |       \
  |                                                              |        +--> Failed
  |                                                              |             (failure/conflict/
  |                                                              |              explicit rollback
  |                                                              |              verification failure)
  |                                                              |
  |                                                              +---- rollback observer unavailable
  |                                                              |     --> [remain RollbackPending]
  |                                                              |         outcome=RollbackVerificationRequired
  |                                                              v
  |                                                          RolledBack
  v
Verified
  |
  v
Finalized
```

`ReconciliationRequired` is deliberately nonterminal. It represents an execution whose external effect cannot yet be classified safely.

Verification uncertainty is also nonterminal, but it does not need a new durable state. If `Action::verify` returns an observer error, the journal stays at `Committed`, evidence records `ExecutionOutcome::VerificationRequired`, and compensation is not started. Recovery re-establishes the output and attempts verification again without repeating `commit`.

Rollback-verification uncertainty follows the same epistemic rule. If `Action::verify_rollback` returns an observer error while post-rollback invariants do not explicitly fail, the journal stays at `RollbackPending`, evidence records `ExecutionOutcome::RollbackVerificationRequired`, and compensation is not repeated solely because the observer is unavailable.

## Normal path

```text
Created -> Proposed -> Validated -> Prepared -> Committed -> Verified -> Finalized
```

The kernel cannot reach `Prepared` until validation, preconditions, snapshot capture, and pre-commit invariants succeed. It cannot reach `Verified` until an effect is established and postconditions independently pass. It cannot durably write a terminal state until sealed evidence has been persisted.

## Verification paths

Verification has three meanings:

```text
Ok(all checks pass) -> Committed -> Verified
Ok(any check fails) -> explicit negative observation -> compensation policy
Err(observer error) -> keep Committed -> VerificationRequired checkpoint
```

An observer outage is never interpreted as proof that the effect is wrong. Repeated observer outages therefore produce repeated recoverable checkpoints with no additional commit and no rollback.

## Compensation paths

A compensable action may report:

```text
RollbackStatus::Succeeded
RollbackStatus::Failed(error)
RollbackStatus::Conflict { reason }
```

`Succeeded` still requires independent rollback verification and post-rollback invariants before `RolledBack` is legal. `Failed` and `Conflict` both terminate as `Failed`/`RollbackFailed`, but evidence distinguishes mechanical rollback failure from deliberate refusal caused by ownership loss.

Rollback verification is itself tri-state in meaning:

```text
Ok(all checks pass) -> RollbackPending -> RolledBack
Ok(any check fails) -> explicit negative rollback observation
Err(observer error) -> keep RollbackPending -> RollbackVerificationRequired checkpoint
```

On recovery from `RollbackPending`, the kernel always verifies first. A passing observation finalizes without another rollback. An observer error remains pending and does not invoke `rollback_status`. Only an explicit negative observation or failed rollback invariant permits the normal compensation retry/conflict path.

The reference SQLite action enforces ownership in an atomic conditional update. The reference file action compares current content before restoring its snapshot; it detects ordinary interference but does not claim distributed filesystem fencing.

## Durable recovery interpretation

| Last durable state | Recovery plan | Kernel behavior |
|---|---|---|
| `Created`, `Proposed`, `Validated` | `AbortBeforeCommit` | terminate without invoking `commit` |
| `Prepared` | `ReconcileBeforeRetry` | use action readback; never blind retry |
| `ReconciliationRequired` | `ReconcileBeforeRetry` | retry reconciliation only, not commit |
| `Committed` | `VerifyOrRollback` | re-establish output through reconciliation, then verify; rollback only after explicit failed verification/invariant |
| `RollbackPending` | `CompleteRollback` | verify first; finalize if complete; stay pending if observer unavailable; retry compensation only after explicit negative evidence |
| `Verified` | `FinalizeVerified` | persist/finalize without repeating the effect |
| `Finalized`, `Rejected`, `Aborted`, `RolledBack`, `Failed` | `None` | settled |

A prior `VerificationRequired` result has last durable state `Committed`, so restart recovery follows the `VerifyOrRollback` plan. If verification is again indeterminate, the durable head remains `Committed`.

A prior `RollbackVerificationRequired` result has last durable state `RollbackPending`, so restart recovery follows `CompleteRollback`. If rollback verification is again indeterminate, the durable head remains `RollbackPending` and rollback count does not increase.

## Durable JSONL framing

Journal and evidence files use newline as the local committed-frame marker. On open:

```text
valid newline-terminated frames + partial final bytes
    -> truncate only partial final bytes

malformed newline-terminated frame
    -> fail closed as committed corruption
```

Thus an interrupted append cannot make earlier committed recovery history unreadable, while true committed corruption is not silently hidden.

## Terminal-state ordering

For K5, terminal durability is ordered as:

```text
construct terminal transition in memory
-> redact and seal ExecutionRecord
-> persist evidence
-> append terminal journal state + fsync
-> return ExecutionResult
```

Therefore a durable terminal journal state implies that its evidence already exists. If evidence persistence fails, the journal remains at a recoverable nonterminal head such as `Verified`, `Committed`, or `RollbackPending`.
