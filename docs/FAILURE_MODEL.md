# Failure Model

The microkernel treats uncertainty as data. A failed transport, a known failed commit, an unknown commit outcome, a failed verification, and a failed rollback are different states with different safe responses.

## Explicit failure classes

Evidence distinguishes:

- validation failure;
- precondition failure;
- snapshot failure;
- known commit failure;
- unknown commit outcome;
- verification failure;
- invariant violation;
- rollback failure;
- compensation unavailable;
- reconciliation failure;
- recovery failure;
- evidence-persistence failure;
- journal-write failure;
- execution-ID conflict;
- idempotency conflict;
- injected fault.

Infrastructure failures are surfaced as typed `ExecutionError` values and are never collapsed into application success.

## Unknown outcome

An unknown commit is **not** a failed commit.

```text
client sends request
server applies effect
response is lost
client observes timeout
```

Blind retry can duplicate the effect. The kernel therefore journals `ReconciliationRequired` and invokes the action's readback strategy. Only `ReconciliationResult::Committed(output)` may continue to postcondition verification. `NotCommitted` terminates as `Aborted`. `Unresolved` remains `ReconciliationRequired` and can later be resumed with `Kernel::recover`; `commit` is not called again.

## Rollback and compensation

Rollback is compensation, not time travel. It may fail, may partially restore state, or may return successfully while the desired prior state is still absent. Therefore:

```text
rollback call returned Ok != rollback verified
```

`ExecutionOutcome::RolledBack` requires all of:

1. the action declares itself compensable;
2. the compensating operation succeeds or recovery proves it already happened;
3. rollback postconditions independently verify;
4. post-rollback invariants pass.

Otherwise the outcome is `RollbackFailed`. If an action declares `CompensationPolicy::NonCompensable`, the kernel does not invoke rollback and records `CompensationUnavailable` explicitly.

## Identity and duplicate execution

Every execution ID is claimed before the lifecycle begins. Duplicate execution IDs fail closed. Idempotency keys are separately claimed before effect execution; duplicate keys do not produce another commit. `FileIdempotencyStore` persists both claim types across restart using atomically created, fsync-backed claim files.

The implementation deliberately chooses fail-closed duplicate handling, which is one of the build brief's permitted behaviors. It does not invent a prior output when the application-specific output cannot safely be reconstructed.

## Crash model

The journal is a local write-ahead record. Important writes are append + flush + `sync_data()`.

```text
PREPARED(snapshot/recovery envelope) + fsync
attempt effect
COMMITTED + fsync
verify
VERIFIED + fsync
persist sealed evidence
terminal state + fsync
```

Recovery interprets the last durable head conservatively:

- `Prepared`: the effect may or may not have occurred -> reconcile;
- `ReconciliationRequired`: still uncertain -> reconcile again, never recommit;
- `Committed`: effect was established -> reconstruct/read back output, then verify or compensate;
- `RollbackPending`: compensation may be complete or interrupted -> verify first, then resume only if required;
- `Verified`: postconditions already passed -> finalize without repeating the effect.

If terminal evidence was persisted but the terminal journal append failed, `Kernel::recover` detects the terminal evidence and fails closed with `AlreadyFinalized` rather than re-running the effect.

## Evidence failure and K5

A terminal journal state is never written before its evidence exists. The terminal transition is first represented in memory, the execution record is redacted/sealed and persisted, and only then is the terminal journal entry appended.

Thus:

```text
terminal_state_durable => evidence_record_exists
```

If evidence persistence fails after verification, the call returns `ExecutionError::EvidencePersistence` and the durable journal remains at nonterminal `Verified`, from which recovery can safely finalize later.

## Secret handling

Evidence may contain adapter-provided diagnostic strings, so it is redacted before hashing and persistence. `ConservativeRedactor` removes explicitly registered secret values and values following common sensitive labels such as `token=`, `password=`, `secret=`, `api_key=`, and `authorization=`. Tests persist deliberately contaminated records and verify that raw values do not appear in JSONL evidence.

The recovery journal stores serialized application snapshots because recovery requires them. A snapshot can itself contain sensitive application data, so journal files must be protected as application state; the kernel does not claim encryption or key management.

## Fault injection matrix

The deterministic injector exposes exactly the required boundaries:

| Fault point | Durable head / interpretation |
|---|---|
| `BeforeSnapshot` | validated; abort before commit |
| `AfterSnapshot` | validated; abort before commit |
| `BeforeCommit` | prepared; reconcile before retry |
| `DuringCommit` | prepared; reconcile before retry |
| `AfterCommit` | prepared; effect may exist, reconcile |
| `BeforeVerify` | committed; verify or rollback |
| `DuringRollback` | rollback pending; complete/verify compensation |
| `AfterRollback` | rollback pending; verify whether compensation already completed |

The action-specific fake remote environment separately models partial external effects, timeout before apply, lost response after apply, duplicate request handling, and unresolved conflicting state.

Every fault point has deterministic coverage and property-generated coverage. Fault injection models abrupt interruption: it intentionally stops the normal lifecycle and leaves the durable journal as the source of recovery truth.
