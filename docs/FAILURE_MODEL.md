# Failure Model

The microkernel treats uncertainty as data. A known failed commit, an unknown commit outcome, an explicit failed postcondition, an unavailable verification observer, a rollback implementation failure, an unavailable rollback-verification observer, and a compensation ownership conflict are distinct conditions with different safe responses.

## Explicit failure classes

Evidence distinguishes validation, precondition, snapshot, known commit, unknown commit, explicit verification, indeterminate verification, invariant, rollback, indeterminate rollback verification, compensation-unavailable, compensation-conflict, reconciliation, recovery, evidence-persistence, journal-write, execution-ID, idempotency, and injected-fault failures.

Infrastructure failures are surfaced as typed `ExecutionError` values and are never collapsed into application success.

## Unknown commit outcome

An unknown commit is **not** a failed commit.

```text
client sends request
server applies effect
response is lost
client observes timeout
```

Blind retry can duplicate the effect. The kernel therefore enters `ReconciliationRequired` and invokes action-specific readback. `Committed(output)` proceeds to verification, `NotCommitted` aborts without retry, and `Unresolved` stays recoverable. The kernel does not invoke `commit` again merely because the outcome is uncertain.

## Verification uncertainty

An unavailable verifier is **not** a failed postcondition.

```text
commit is established
verification readback times out / observer fails
world state is unknown
```

`Action::verify` is interpreted as:

```text
Ok(all checks pass) -> verified
Ok(any check fails) -> explicit verification failure
Err(error) -> VerificationIndeterminate
```

On `Err`, the kernel records `FailureClass::VerificationIndeterminate`, `VerificationRecord.indeterminate = true`, and `ExecutionOutcome::VerificationRequired`. The durable journal remains at `Committed`; no rollback occurs. Recovery re-establishes output through reconciliation and attempts verification again without repeating the commit.

This enforces K8:

```text
verification_indeterminate
=> no automatic compensation
   AND outcome != Success
   AND execution remains recoverable
```

## Rollback and compensation

Rollback is compensation, not time travel. It may fail, may partially restore state, or may become unsafe because another actor has modified the world after this execution's commit.

`RollbackStatus` therefore distinguishes:

```text
Succeeded
Failed(error)
Conflict { reason }
```

`Succeeded` does not itself prove rollback correctness. `ExecutionOutcome::RolledBack` still requires rollback postconditions and post-rollback invariants to pass.

`Conflict` means the action deliberately refused to restore its snapshot because ownership of the current state could not be established. The kernel records `FailureClass::CompensationConflict` and never claims rollback success.

The SQLite reference action performs compensation with an atomic conditional `UPDATE` that matches the exact balance/version produced by its commit. If another writer has moved the row forward, zero rows are changed and the newer state is preserved.

The atomic-file reference action re-reads the target and restores the snapshot only if the current bytes still equal the bytes written by this execution. This detects ordinary intervening changes, but filesystems do not provide a universal compare-and-swap primitive here; an external write can race between comparison and replacement, and byte-identical independent writes are indistinguishable without additional fencing/versioning. The example therefore demonstrates conflict-aware compensation rather than distributed ownership guarantees.

## Rollback verification uncertainty

An unavailable rollback verifier is **not** proof that compensation failed, and it is not permission to execute compensation again.

```text
rollback hook reports success
rollback verification observer times out / fails
current compensation result cannot be established
```

`Action::verify_rollback` is interpreted as:

```text
Ok(all checks pass) -> rollback verified
Ok(any check fails) -> explicit negative rollback observation
Err(error) -> RollbackVerificationIndeterminate
```

When `Err` occurs and post-rollback invariants do not independently fail, the kernel records `FailureClass::RollbackVerificationIndeterminate`, `RollbackRecord.verification_indeterminate = true`, and `ExecutionOutcome::RollbackVerificationRequired`. The durable journal remains at `RollbackPending`.

Recovery from `RollbackPending` is verify-before-act. The kernel attempts `verify_rollback` before invoking `rollback_status` again. If observation is still unavailable and rollback invariants remain non-negative evidence, it persists another nonterminal checkpoint and does not increase rollback count. If observation later passes, the execution finalizes as `RolledBack` without another compensation. Only an explicit negative rollback observation or explicit rollback-invariant failure permits the normal compensation retry/conflict path.

This enforces K11:

```text
rollback_verification_indeterminate
=> durable_head = RollbackPending
   AND outcome = RollbackVerificationRequired
   AND observer_error_alone_does_not_repeat_compensation
```

## Crash model and torn writes

The journal is a local write-ahead record. Durable JSONL records are written as:

```text
serialized JSON
newline
flush
sync_data
```

The newline is the local frame-commit marker. If a process or host interruption leaves unterminated bytes after the final newline, reopening the journal/evidence store truncates only that uncommitted tail and syncs the repair. Earlier committed frames remain readable.

A malformed **newline-terminated** record is different: it is treated as committed corruption and parsing fails closed. The implementation never scans past or silently discards a corrupted committed frame.

New durable JSONL files are synced on creation; on Unix, the parent directory is also synced so the directory entry is durable before later records are relied on.

The principal execution order remains:

```text
PREPARED(snapshot/recovery envelope) + fsync
attempt effect
COMMITTED + fsync
verify
VERIFIED + fsync
persist sealed evidence
terminal state + fsync
```

Compensation adds its own uncertainty boundary:

```text
ROLLBACK_PENDING + fsync
attempt compensation
verify compensation
  -> verified: terminal RolledBack
  -> explicit negative: failure/retry/conflict semantics
  -> observer unavailable: remain RollbackPending
```

Recovery interprets `Prepared` as effect-uncertain, `Committed` as effect-established but not necessarily verified, `RollbackPending` as compensation potentially interrupted or awaiting trustworthy observation, and `Verified` as safe to finalize without repeating the effect.

## Identity and duplicate execution

Every execution ID is claimed before the lifecycle begins. Duplicate execution IDs fail closed. Idempotency keys are separately claimed before effect execution; duplicate keys do not produce another local commit. `FileIdempotencyStore` persists both claim types using atomically created, fsync-backed files.

This is a local execution guarantee, not a distributed exactly-once protocol.

## Evidence failure and terminal ordering

A terminal journal state is never written before its evidence exists:

```text
terminal transition in memory
-> redact record
-> seal record
-> persist evidence
-> append terminal state
```

If evidence persistence fails, the durable journal remains nonterminal and recovery can continue safely. Schema-version-4 evidence records both verification indeterminacy and rollback-verification indeterminacy explicitly. `RollbackRecord.verification_indeterminate` is serde-defaulted so older records without the field remain decodable.

## Secret handling

Evidence is redacted before hashing and persistence. The default redactor handles explicitly registered secrets plus common sensitive labels. Recovery journal snapshots are application state and may contain sensitive data; the kernel does not claim encryption or key management.

## Fault and regression matrix

The deterministic injector still covers:

| Fault point | Durable interpretation |
|---|---|
| `BeforeSnapshot` | validated; abort before commit |
| `AfterSnapshot` | validated; abort before commit |
| `BeforeCommit` | prepared; reconcile before retry |
| `DuringCommit` | prepared; reconcile before retry |
| `AfterCommit` | prepared; effect may exist; reconcile |
| `BeforeVerify` | committed; recover and verify |
| `DuringRollback` | rollback pending; verify-before-resume compensation |
| `AfterRollback` | rollback pending; verify whether compensation already completed |

Hardening regressions additionally prove:

- verifier errors do not trigger rollback;
- repeated verifier errors remain recoverable;
- later successful verification does not repeat commit;
- later explicit failed verification may compensate;
- rollback-verifier errors after compensation do not become terminal failure;
- repeated rollback-verifier errors do not repeat compensation;
- later rollback verification can finalize without another compensation;
- explicit negative rollback observation can enter the normal compensation retry path;
- torn final journal/evidence frames do not destroy prior history;
- malformed committed frames fail closed;
- appends remain valid after torn-tail repair;
- compensation conflicts are explicit;
- newer SQLite state and conflicting file content are preserved rather than blindly overwritten.
