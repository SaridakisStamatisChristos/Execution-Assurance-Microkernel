# Failure Model

## Explicit classes

The evidence layer distinguishes validation, precondition, snapshot, commit, unknown-outcome, verification, invariant, rollback, reconciliation, evidence-persistence, journal, idempotency, and injected-fault failures.

Infrastructure failures in the journal, evidence store, idempotency store, or test fault injector are returned as typed `ExecutionError` values rather than collapsed into application failure.

## Unknown outcome

An unknown commit is not a failed commit. Example:

```text
client POSTs payment
server commits payment
response is lost
client observes timeout
```

Blind retry can duplicate the side effect. The kernel therefore moves through `ReconciliationRequired` and invokes the action's readback/reconciliation strategy. Only a reconciled `Committed(output)` may continue to verification. `NotCommitted` aborts. `Unresolved` returns `ReconciliationRequired` with evidence.

## Rollback

Rollback is compensation, not time travel. It may fail, may only partially restore state, or may violate invariants. Therefore:

```text
rollback call returned Ok != rollback succeeded
```

`RolledBack` is produced only after independent rollback verification and post-rollback invariant checks pass. Otherwise the result is `RollbackFailed`.

## Evidence-store failure

If the effect has already been verified but evidence persistence fails, the kernel returns `ExecutionError::EvidencePersistence` rather than `Success`. The durable journal still indicates the last lifecycle state and can be inspected operationally.

## Fault injection

The test injector defines boundaries before/after snapshot, before/during/after commit, before verify, and during/after rollback. Faults intentionally interrupt the lifecycle without manufacturing a terminal record; the journal is then used to derive a recovery directive. This models process death rather than a normal handled application error.
