# State Machine

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
  |   +---- unknown commit ----> ReconciliationRequired ----> Aborted (confirmed not committed)
  |                                      |
  |                                      +-------------------> Committed (confirmed committed)
  v
Committed
  |  \
  |   +---- verification/invariant failure ----> RollbackPending ----> RolledBack
  |                                                        \
  |                                                         +--------> Failed
  v
Verified
  |
  v
Finalized
```

Only transitions encoded by `ExecutionState::can_transition_to` are legal.

## Durable recovery interpretation

| Last durable state | Required recovery plan |
|---|---|
| `Created`, `Proposed`, `Validated` | abort before commit |
| `Prepared` | reconcile before retry |
| `Committed` | verify or rollback |
| `RollbackPending` | complete rollback |
| `Verified` | finalize verified execution |
| terminal/settled states | none |

`Prepared` is intentionally conservative: a crash can occur after the remote side effect but before the local `Committed` journal append.
