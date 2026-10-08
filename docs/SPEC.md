# Execution Assurance Microkernel Specification

## 1. Scope

The kernel mediates already-proposed externally visible actions. Planning, business policy, authentication, distributed consensus, agents, LLMs, workflow orchestration, message brokers, cloud infrastructure, vector databases, plugin systems, and UI are out of scope.

Its claim is deliberately narrow:

> A minimal reference implementation that distinguishes attempted execution from verified execution.

## 2. Semantic definition of success

For an execution `e`:

```text
Success(e) := CommitEstablished(e) AND PostconditionsVerified(e)
```

`CommitEstablished` means either the commit returned `Confirmed(output)` or reconciliation independently established that the effect exists. A transport success, method return, HTTP 2xx, or database driver `Ok` is not sufficient evidence of semantic success.

## 3. Public execution boundary

Application code implements `Action`; the kernel owns execution order. The effectful hooks:

```text
commit
rollback
reconcile
```

all require `&EffectPermit`. The type is public so downstream implementations can name it, but its constructor/field are crate-private. Safe downstream Rust therefore cannot construct the capability required to invoke those hooks directly. A compile-fail doctest enforces that boundary.

The public execution entry points are intentionally small:

```text
Kernel::execute
Kernel::execute_idempotent
Kernel::execute_request
Kernel::recover
```

## 4. Lifecycle ownership

For a new execution, the kernel performs:

1. choose or accept an `ExecutionId`;
2. claim the execution ID fail-closed;
3. journal `Proposed`;
4. claim optional `IdempotencyKey`;
5. validate action semantics;
6. evaluate explicit preconditions;
7. journal `Validated`;
8. capture rollback snapshot;
9. evaluate invariants before commit;
10. serialize the recovery envelope and journal `Prepared` with fsync;
11. attempt the effect exactly once through `commit`;
12. if the outcome is unknown, reconcile before any retry decision;
13. journal `Committed` once the effect is established;
14. evaluate post-commit invariants;
15. independently verify postconditions;
16. journal `Verified` when verification passes;
17. if verification/invariants fail and compensation exists, enter `RollbackPending`, compensate, verify rollback, and re-check invariants;
18. redact and SHA-256-seal the execution record;
19. persist evidence;
20. only after evidence exists, durably append the terminal state.

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

Only transitions encoded by `ExecutionState::can_transition_to` are legal. `ReconciliationRequired` is nonterminal because the uncertainty may later be resolved through `Kernel::recover`.

## 6. Preconditions and invariants

Preconditions and invariants return `CheckRecord` values containing:

```text
name
passed
reason
```

rather than bare booleans. Invariants may be evaluated at:

```text
BeforeCommit
AfterCommit
AfterRollback
```

This makes both decisions and evidence inspectable.

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

The ordering is structural in `Kernel`; debug assertions additionally encode the pre-commit condition.

### K2 — No successful result without verification

```text
outcome = Success
=> commit_established AND verification_passed
```

The normal success path cannot call terminal finalization until `Verified` has been reached.

### K3 — Failed verification cannot appear successful

```text
verification_failed => outcome != Success
```

A compensable action enters rollback. A non-compensable action records `CompensationUnavailable` and fails explicitly.

### K4 — Duplicate identity cannot produce another effect

```text
same_execution_id => second execution rejected before lifecycle
same_idempotency_key => commit_count <= 1
```

`InMemoryIdempotencyStore` provides process-local claims. `FileIdempotencyStore` persists execution-ID and idempotency-key claims using atomic creation and fsync so the guarantee survives restart on the local filesystem.

### K5 — Durable terminal state implies durable evidence

```text
terminal_state_durable => evidence_record_exists
```

The terminal transition is first made in memory. The execution record is redacted, sealed, and persisted. Only then is the terminal journal state appended. Evidence-store failure therefore leaves the journal at a nonterminal recoverable state.

### K6 — Rollback success requires rollback verification

```text
outcome = RolledBack
=> compensation_succeeded_or_already_complete
   AND rollback_postconditions_verified
   AND rollback_invariants_passed
```

A partially applied or unverifiable rollback becomes `RollbackFailed`.

### K7 — Unknown commit outcome cannot trigger blind retry

```text
commit_unknown => reconcile_before_any_retry
```

The kernel never invokes `commit` twice for the same execution. Restart recovery from `Prepared` and `ReconciliationRequired` calls only the action's reconciliation/readback logic.

## 8. Commit outcomes

`Action::commit` returns:

```text
CommitStatus::Confirmed(output)
CommitStatus::Failed(error)
CommitStatus::Unknown { reason }
```

`Unknown` is first-class. It moves the execution to `ReconciliationRequired`. Reconciliation returns:

```text
Committed(output)
NotCommitted
Unresolved { reason }
```

Only `Committed(output)` proceeds to normal verification. `NotCommitted` aborts without retry. `Unresolved` remains recoverable and fail-closed.

## 9. Compensation

Every action declares:

```text
CompensationPolicy::Compensable
CompensationPolicy::NonCompensable
```

A compensable action supplies snapshot-based rollback and independent rollback verification. A non-compensable action is never described as rolled back; a post-commit verification failure records the absence of a safe compensation path explicitly.

## 10. Evidence

`ExecutionRecord` contains:

- schema version;
- execution/action/idempotency identity;
- start and completion timestamps;
- state transitions;
- validation and precondition checks;
- invariants before commit, after commit, and after rollback;
- commit disposition;
- reconciliation result;
- verification result;
- rollback result;
- recovery metadata;
- failure class/message;
- final outcome;
- SHA-256 record hash.

The hash is calculated over canonical serialized record data with `record_hash = null`. `verify_hash()` detects mutation.

Before sealing, `EvidenceRedactor` is applied. The default `ConservativeRedactor` supports explicitly registered secret values and redacts values following common sensitive labels. `JsonlEvidenceStore` appends, flushes, and calls `sync_data()`.

## 11. Journal and recovery envelope

`FileJournal` stores JSONL records with append + flush + `sync_data()`.

The `Prepared` entry additionally stores a `RecoveryEnvelope` containing:

```text
action_id
action_type
idempotency_key
started_at_ms
compensation_policy
serialized rollback snapshot
```

This is intentionally application state, not audit evidence. Applications must protect journal storage appropriately.

## 12. Recovery algorithm

`RecoveryManager::scan` reconstructs the latest durable state and most recent recovery envelope for each execution. `Kernel::recover(execution_id, action, context)` checks action identity and executes the corresponding safe plan.

| Durable head | Plan |
|---|---|
| `Created`/`Proposed`/`Validated` | abort without commit |
| `Prepared` | reconcile before retry |
| `ReconciliationRequired` | reconcile again; never commit |
| `Committed` | reconstruct/read back output, then verify or compensate |
| `RollbackPending` | verify whether rollback completed; otherwise resume it |
| `Verified` | finalize without repeating effect |
| terminal state | no recovery action |

Recovery also checks persisted evidence first. If a terminal evidence record exists but the terminal journal append was lost, the kernel returns `AlreadyFinalized` instead of executing anything again.

## 13. Fault model

The deterministic `FaultInjector` exposes:

```text
BeforeSnapshot
AfterSnapshot
BeforeCommit
DuringCommit
AfterCommit
BeforeVerify
DuringRollback
AfterRollback
```

Every point is covered by deterministic tests and property-generated selection. The injected fault models abrupt interruption: normal finalization stops and the durable journal head becomes the recovery source of truth.

`DuringCommit` is the kernel boundary immediately before entering the application commit hook; action-specific partial external effects are modeled separately by the adversarial fake remote API.

## 14. Deterministic failure matrix

Automated tests cover at least:

- crash before commit;
- crash during commit boundary;
- commit succeeds but response disappears;
- verification failure;
- rollback failure and partial rollback failure;
- journal write failure;
- evidence write failure;
- idempotency-store failure;
- duplicate execution ID;
- duplicate idempotency key;
- invariant violation after commit;
- non-compensable verification failure;
- restart recovery from `Prepared`, `Committed`, `RollbackPending`, and `Verified`;
- evidence tampering and secret redaction;
- adversarial remote timeout, lost response, duplicate request, partial effect, and conflicting state.

## 15. Reference actions

The repository intentionally contains exactly three strong executable reference actions:

1. atomic file replacement;
2. optimistic SQLite mutation;
3. adversarial fake remote API.

They demonstrate the same kernel contract across local filesystem, database, and unreliable remote-effect boundaries.

## 16. Benchmarks

`benches/kernel.rs` separates:

- core verified success;
- core verification failure followed by verified rollback;
- durable success using fsync-backed journal, evidence, and identity stores.

Criterion is used for repeatable measurements. CI compiles the benchmark target on every change. Performance results are environment-specific and must not be treated as universal latency guarantees.

## 17. Non-goals and limits

The kernel does **not** claim exactly-once delivery across arbitrary distributed systems. Durable local identity claims protect the local execution boundary, while remote exactly-once behavior still depends on the remote system exposing stable identity/readback or idempotency semantics.

Recovery requires the caller to re-supply the matching `Action` implementation and current application context. The kernel persists the rollback snapshot and identities required to reason safely but does not serialize arbitrary Rust code or own application resource discovery.

The design intentionally favors explicit unresolved states over unsafe inference.
