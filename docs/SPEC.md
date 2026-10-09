# Execution Assurance Microkernel Specification

## 1. Scope

The kernel mediates already-proposed externally visible actions. Planning, business policy, authentication, distributed consensus, agents, LLMs, workflow orchestration, message brokers, cloud infrastructure, vector databases, plugin systems, and UI are out of scope.

Its claim is deliberately narrow:

> A minimal reference implementation that distinguishes attempted execution from verified execution.

The implementation prefers explicit uncertainty and explicit ownership loss over unsafe inference.

## 2. Semantic definition of success

For an execution `e`:

```text
Success(e) := CommitEstablished(e) AND PostconditionsVerified(e)
```

`CommitEstablished` means either the commit returned `Confirmed(output)` or reconciliation independently established that the effect exists. A transport success, method return, HTTP 2xx, or database driver `Ok` is not sufficient evidence of semantic success.

Verification is also epistemic: an explicit failed postcondition is different from an unavailable observer. The latter remains recoverable and never causes automatic compensation.

## 3. Public execution boundary

Application code implements `Action`; the kernel owns execution order. The effectful hooks `commit`, `rollback`, and `reconcile` require `&EffectPermit`. The type is public so downstream implementations can name it, but its constructor and field are crate-private. Safe downstream Rust therefore cannot construct the capability required to invoke those hooks directly. A compile-fail doctest enforces that boundary.

The public execution entry points are intentionally small:

```text
Kernel::execute
Kernel::execute_idempotent
Kernel::execute_request
Kernel::recover
```

## 4. Lifecycle ownership

For a new execution, the kernel:

1. chooses or accepts an execution ID;
2. claims the execution ID fail-closed;
3. journals `Proposed`;
4. claims an optional idempotency key;
5. validates action semantics and preconditions;
6. journals `Validated`;
7. captures a rollback snapshot and checks pre-commit invariants;
8. serializes the recovery envelope and journals `Prepared` durably;
9. attempts the effect once through `commit`;
10. reconciles any unknown commit outcome before any retry decision;
11. journals `Committed` once the effect is established;
12. checks post-commit invariants;
13. independently verifies postconditions;
14. if verification passes, journals `Verified` and finalizes;
15. if verification explicitly disproves the postcondition, applies compensation policy;
16. if the verification observer itself fails, records `VerificationRequired` while leaving the durable head at `Committed` and performs no rollback;
17. if compensation is attempted, distinguishes success, implementation failure, and ownership conflict;
18. verifies successful compensation and post-rollback invariants;
19. redacts and seals evidence;
20. persists evidence before writing any terminal journal state.

## 5. State machine

Nominal path:

```text
Created -> Proposed -> Validated -> Prepared -> Committed -> Verified -> Finalized
```

Failure/recovery states include:

```text
Rejected
Aborted
ReconciliationRequired
RollbackPending
RolledBack
Failed
```

`ReconciliationRequired` is nonterminal. Verification-observer uncertainty does not require a new durable state: the journal remains at `Committed`, evidence records `VerificationRequired`, and recovery re-establishes output before verification is attempted again.

Only transitions encoded by `ExecutionState::can_transition_to` are legal.

## 6. Preconditions and invariants

Preconditions and invariants return `CheckRecord { name, passed, reason }` rather than bare booleans. Invariants may be evaluated at `BeforeCommit`, `AfterCommit`, and `AfterRollback`.

## 7. Kernel invariants

### K1 — No unchecked commit

```text
commit_attempted
=> validation_passed
   AND preconditions_passed
   AND snapshot_captured
   AND invariants_before_passed
   AND state = Prepared
```

### K2 — No successful result without verification

```text
outcome = Success
=> commit_established AND verification_passed
```

### K3 — Failed verification cannot appear successful

```text
verification_failed => outcome != Success
```

An explicit failed check may trigger compensation. An unavailable verifier is governed by K8 instead.

### K4 — Duplicate identity cannot produce another effect

```text
same_execution_id => second execution rejected before lifecycle
same_idempotency_key => commit_count <= 1
```

`FileIdempotencyStore` persists local execution-ID and idempotency claims using atomic creation and fsync.

### K5 — Durable terminal state implies durable evidence

```text
terminal_state_durable => evidence_record_exists
```

Evidence is redacted, sealed, and persisted before the terminal journal append.

### K6 — Rollback success requires rollback verification

```text
outcome = RolledBack
=> compensation_succeeded_or_already_complete
   AND rollback_postconditions_verified
   AND rollback_invariants_passed
```

### K7 — Unknown commit outcome cannot trigger blind retry

```text
commit_unknown => reconcile_before_any_retry
```

The kernel never invokes `commit` twice for one in-flight execution merely because the outcome is uncertain.

### K8 — Indeterminate verification cannot trigger compensation

```text
verification_observer_error
=> outcome = VerificationRequired
   AND rollback_not_started
   AND outcome != Success
   AND durable_head = Committed
```

`Action::verify` has three semantic interpretations:

```text
Ok(all checks pass) -> verified
Ok(any check fails) -> explicit verification failure
Err(observer error) -> verification indeterminate
```

The third case is not evidence that the postcondition is false. Recovery may retry observation after re-establishing the committed output, but it does not repeat the commit.

### K9 — Torn uncommitted log tails cannot invalidate prior committed history

The durable JSONL stores use newline as the local frame-commit marker. On open, an unterminated final byte sequence is treated as an interrupted, uncommitted tail and truncated to the last newline. Earlier newline-terminated frames remain available.

```text
torn_final_uncommitted_frame
=> prior_committed_frames_remain_readable
```

A malformed **newline-terminated** frame is not repaired or ignored; readers fail closed because that represents committed corruption rather than a recognizable torn tail.

### K10 — Compensation exposes ownership loss instead of silently overwriting it

`RollbackStatus` distinguishes:

```text
Succeeded
Failed(error)
Conflict { reason }
```

An action that can detect that its committed state has been replaced by another actor returns `Conflict`, which becomes `FailureClass::CompensationConflict`; the kernel does not claim rollback success.

Ownership checks are action-specific. The SQLite reference action enforces the check atomically in the conditional `UPDATE`. The file reference action re-reads and compares current content before restoration; ordinary conflicting changes are refused, but a filesystem without compare-and-swap/fencing cannot make a universal distributed ownership guarantee across the check-and-replace interval.

## 8. Commit outcomes and reconciliation

`Action::commit` returns:

```text
CommitStatus::Confirmed(output)
CommitStatus::Failed(error)
CommitStatus::Unknown { reason }
```

`Unknown` moves execution to `ReconciliationRequired`. Reconciliation returns `Committed(output)`, `NotCommitted`, or `Unresolved { reason }`. Only `Committed` proceeds to verification; `NotCommitted` aborts without retry; `Unresolved` remains recoverable.

## 9. Verification semantics

Successful commit establishment is necessary but insufficient for success. Verification should independently observe the resulting world.

A verifier error is represented in schema-version-3 evidence as `VerificationRecord { passed: false, indeterminate: true, ... }` with `FailureClass::VerificationIndeterminate` and `ExecutionOutcome::VerificationRequired`. The durable journal head remains `Committed` so restart recovery can safely re-establish output through reconciliation and attempt verification again.

An explicit failed check has `indeterminate = false`; it is a real negative observation and may enter compensation.

## 10. Compensation

Every action declares `Compensable` or `NonCompensable`. Existing actions may implement `rollback` only; the default `rollback_status` maps `Ok` to `Succeeded` and `Err` to `Failed`, preserving source-level behavior.

Actions that can detect interference override `rollback_status` and return `Conflict`. A conflict is terminally recorded as a failed compensation attempt rather than overwriting newer state or claiming rollback success.

## 11. Evidence

`ExecutionRecord` schema version 3 contains execution/action/idempotency identity, timestamps, transitions, validation/precondition/invariant checks, commit disposition, reconciliation, verification including indeterminate status, rollback, recovery metadata, failure classification, final outcome, and a SHA-256 record hash.

Before sealing, `EvidenceRedactor` runs. `JsonlEvidenceStore` appends a newline-delimited frame, flushes, and calls `sync_data()`.

## 12. Durable journal and JSONL framing

`FileJournal` and `JsonlEvidenceStore` share the same local framing rule:

```text
serialized JSON bytes
newline                <- frame commit marker
flush
sync_data
```

When a previously existing file is opened, bytes after the final newline are truncated as an uncommitted torn tail and the repair is synced. A complete newline-terminated record is never silently discarded; malformed committed JSON causes a read error.

Creation of a new durable JSONL file is `sync_all`'d and, on Unix, its parent directory is synced so the new directory entry is durable before later frames are relied on.

The `Prepared` journal entry carries the recovery envelope containing action identity, idempotency identity, start time, compensation policy, and serialized rollback snapshot.

## 13. Recovery algorithm

| Durable head | Plan |
|---|---|
| `Created`/`Proposed`/`Validated` | abort without commit |
| `Prepared` | reconcile before retry |
| `ReconciliationRequired` | reconcile again; never commit |
| `Committed` | re-establish/read back output, then verify; compensate only after explicit failed verification/invariant |
| `RollbackPending` | verify whether rollback completed; otherwise resume it |
| `Verified` | finalize without repeating effect |
| terminal state | no recovery action |

A prior `VerificationRequired` result has durable head `Committed`, so it naturally follows the `Committed` recovery plan. Repeated observer failure remains recoverable and neither increments commit count nor begins compensation.

If terminal evidence exists but the terminal journal append was lost, recovery returns `AlreadyFinalized` rather than repeating the effect.

## 14. Fault and adversarial test model

The deterministic fault injector covers `BeforeSnapshot`, `AfterSnapshot`, `BeforeCommit`, `DuringCommit`, `AfterCommit`, `BeforeVerify`, `DuringRollback`, and `AfterRollback`.

Additional deterministic regressions cover:

- verifier unavailable after a confirmed commit;
- repeated verifier unavailability across recovery;
- later verification success without second commit;
- later explicit verification failure followed by compensation;
- torn final journal and evidence frames;
- committed malformed JSON failing closed;
- append/reopen after tail repair;
- compensation ownership conflict;
- SQLite concurrent-state preservation;
- file-state interference refusal.

## 15. Reference actions

The repository intentionally contains exactly three reference actions:

1. atomic file replacement, including content-based compensation conflict refusal;
2. optimistic SQLite mutation, including atomic conditional compensation;
3. adversarial fake remote API.

The file example cannot distinguish an independent writer that produces byte-identical content and does not claim filesystem fencing or distributed compare-and-swap.

## 16. Benchmarks

`benches/kernel.rs` separates core verified success, explicit verification failure followed by verified rollback, and durable fsync-backed success. CI compiles the benchmark target on every change. Published measurements are environment-specific, not universal latency guarantees.

## 17. Non-goals and limits

The kernel does not claim distributed exactly-once execution, consensus, distributed fencing, universal rollback, encrypted persistence, cryptographic non-repudiation, or automatic reconstruction of arbitrary application code/resources.

Recovery requires the caller to re-supply the matching action and current context. Durable journal snapshots are application state and may contain sensitive data.

The design intentionally favors explicit unresolved or conflicted states over unsafe inference.
