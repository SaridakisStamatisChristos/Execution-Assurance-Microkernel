# Execution Assurance Microkernel Specification

## 1. Scope

The kernel mediates already-proposed externally visible actions. Planning, business policy, authentication, distributed consensus, and orchestration are out of scope.

## 2. Semantic definition of success

For an execution `e`:

```text
Success(e) := CommitConfirmed(e) AND PostconditionsVerified(e)
```

A transport success, method return, HTTP 2xx, or database driver `Ok` is not sufficient evidence of semantic success.

## 3. Lifecycle ownership

The kernel executes this order:

1. create execution identity and metadata;
2. claim optional idempotency key;
3. validate action semantics;
4. evaluate explicit preconditions;
5. capture rollback snapshot;
6. evaluate invariants before commit;
7. durably journal `Prepared`;
8. attempt commit;
9. if unknown, reconcile before any retry decision;
10. durably journal `Committed` once the effect is established;
11. evaluate post-commit invariants;
12. independently verify postconditions;
13. rollback on failed verification/invariant;
14. independently verify rollback and invariants;
15. persist a SHA-256-sealed execution record.

## 4. Kernel invariants

### K1 — No unchecked commit

```text
commit_attempted => validation_passed AND preconditions_passed AND snapshot_captured AND invariants_before_passed
```

### K2 — No successful result without verification

```text
outcome = Success => commit_established AND verification_passed
```

### K3 — Failed verification cannot appear successful

```text
verification_failed => outcome != Success
```

### K4 — Duplicate idempotency keys never produce multiple commits

The provided in-memory registry uses first-claim ownership and fails subsequent executions closed.

```text
same_key => commit_count <= 1
```

### K5 — Every completed execution has evidence

The kernel seals and persists its execution record before returning an `ExecutionResult`. Evidence-store failure is surfaced as `ExecutionError::EvidencePersistence`; the kernel does not return a false successful result.

### K6 — Rollback must itself be verified

```text
outcome = RolledBack => rollback_effect_succeeded AND rollback_postconditions_verified AND rollback_invariants_passed
```

### K7 — Unknown commit outcomes cannot trigger blind retry

```text
commit_unknown => reconcile_before_retry
```

The kernel never invokes `commit` twice during one execution.

## 5. Evidence

Each `ExecutionRecord` contains action identity, timing, all state transitions, validation/precondition/invariant checks, commit disposition, reconciliation, verification, rollback, failure class, outcome, and a SHA-256 hash over the canonical serialized record with `record_hash = null`.

Records are immutable after sealing. The default in-memory store is intended for embedding/tests; `JsonlEvidenceStore` performs append, flush, and `sync_data()` for durable local evidence.

## 6. Durability boundary

`FileJournal::append` serializes one JSON entry per line, flushes, then calls `sync_data()`. The important uncertainty boundary is:

```text
write Prepared + fsync
attempt external effect
write Committed + fsync
```

A crash after the effect but before the second durable write is therefore represented conservatively as `Prepared`, which requires reconciliation before retry.

## 7. Non-goals

The kernel does not claim exactly-once delivery across arbitrary distributed systems. Idempotency and reconciliation depend on the application/remote system exposing enough identity and readback semantics to establish what happened.
