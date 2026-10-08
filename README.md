# Execution Assurance Microkernel

[![CI](https://github.com/SaridakisStamatisChristos/Execution-Assurance-Microkernel/actions/workflows/ci.yml/badge.svg)](https://github.com/SaridakisStamatisChristos/Execution-Assurance-Microkernel/actions/workflows/ci.yml)

A compact Rust reference implementation for one idea:

> **Commit is not success. Success is an established commit plus independently verified postconditions.**

The microkernel owns the lifecycle of an externally effectful action:

```text
PROPOSE -> VALIDATE -> PREPARE -> COMMIT -> VERIFY -> RECORD
                                      |         |
                                      |         +-- failure --> ROLLBACK -> VERIFY ROLLBACK
                                      +-- unknown --> RECONCILE (never blind retry)
```

It does **not** decide what action to take. It decides whether a proposed action may execute, whether the external effect can be established, whether the intended postcondition actually holds, how an interrupted execution is recovered, and what evidence exists afterward.

## Guarantees

- explicit state machine; illegal transitions are rejected;
- preconditions emit inspectable evidence rather than booleans;
- rollback snapshot is captured and durably journaled before the effect boundary;
- invariants are checked before commit, after commit, and after rollback;
- a successful transport/return value is insufficient without independent verification;
- rollback is independently verified and rollback failure is explicit;
- actions explicitly declare whether compensation exists;
- execution IDs and idempotency keys fail closed on duplicates;
- durable local identity/idempotency claims survive process restart;
- unknown commit outcomes enter reconciliation and are never blindly retried;
- an fsync-backed journal carries the recovery envelope needed after restart;
- `Kernel::recover` resumes `Prepared`, `Committed`, `RollbackPending`, `Verified`, and unresolved reconciliation heads without re-running `commit`;
- terminal journal states are written only after their evidence record is durably persisted;
- execution records are SHA-256 sealed and diagnostic evidence is redacted before persistence;
- effectful `commit`, `rollback`, and `reconcile` hooks require a kernel-created `EffectPermit`, so downstream safe code cannot construct the capability needed to bypass the lifecycle;
- all defined fault points have deterministic tests, with additional property/model testing over randomized lifecycle choices.

## Tiny API

Normal execution:

```rust
let result = Kernel::default().execute(action, &mut context)?;
```

Explicit execution identity:

```rust
let request = ExecutionRequest::new(action)
    .with_execution_id("job-42")
    .with_idempotency_key(IdempotencyKey::from("request-42"));
let result = kernel.execute_request(request, &mut context)?;
```

Restart recovery:

```rust
let result = kernel.recover("job-42", action, &mut context)?;
```

Application code implements `Action`. The kernel alone can create the `EffectPermit` required by effectful hooks, so normal safe Rust code cannot invoke `commit`, `rollback`, or `reconcile` as an alternative execution path.

## Failure semantics

| Situation | Kernel outcome |
|---|---|
| validation/precondition fails | `Rejected` |
| snapshot/pre-commit invariant fails | `Aborted` |
| commit known to fail | `CommitFailed` |
| commit outcome is unknown and readback proves no effect | `Aborted` |
| commit outcome is unknown and readback proves effect | continue to verification |
| commit outcome remains unknown | `ReconciliationRequired` |
| verification/invariant fails and compensation verifies | `RolledBack` |
| rollback/rollback verification fails | `RollbackFailed` |
| verification fails for a non-compensable action | explicit `CompensationUnavailable` failure; never reported as rollback success |
| established commit + postconditions verify | `Success` |

`ExecutionOutcome::Success` is therefore materially stronger than “the call returned `Ok`.”

## Crash safety

`FileJournal` appends JSON records, flushes, and calls `sync_data()`. `Prepared` contains a recovery envelope with action identity, idempotency identity, compensation policy, start time, and the serialized rollback snapshot.

The central uncertainty boundary is:

```text
WRITE PREPARED + fsync
ATTEMPT EXTERNAL EFFECT
WRITE COMMITTED + fsync
VERIFY POSTCONDITION
WRITE VERIFIED + fsync
PERSIST SEALED EVIDENCE
WRITE TERMINAL STATE + fsync
```

A crash after the effect but before `Committed` therefore leaves a durable `Prepared` head. Recovery reconciles the external world before any retry decision; it never calls `commit` again. `Committed` is re-established through reconciliation and then verified or compensated. `RollbackPending` resumes or verifies compensation. `Verified` finalizes without a second effect.

If terminal evidence is persisted but the terminal journal write itself fails, recovery finds the terminal evidence and fails closed instead of executing the action again.

## Identity and idempotency

`ExecutionRequest` may supply a deterministic execution ID. Duplicate execution IDs are rejected before the lifecycle starts. `IdempotencyKey` is independently claimed before action execution. `InMemoryIdempotencyStore` is useful for embedding/tests; `FileIdempotencyStore` uses atomic claim files plus fsync so claims survive restart.

A duplicate request is never silently re-executed. This implementation chooses the handoff's permitted **fail-closed** behavior rather than attempting to synthesize an application output from prior evidence.

## Evidence

Each `ExecutionRecord` contains identity, timestamps, transitions, validation/precondition/invariant checks, commit disposition, reconciliation, verification, rollback, recovery metadata, failure classification, outcome, and a SHA-256 seal.

`ConservativeRedactor` removes configured secret values and values following common sensitive labels (`password=`, `token=`, `secret=`, `api_key=`, `authorization=`) before sealing/persistence. Recovery snapshots live in the journal rather than the evidence record; deployments must protect journal storage because application snapshots may themselves be sensitive.

## Reference actions

Exactly three executable reference actions are kept intentionally:

- `cargo run --example atomic_file` — atomic file replacement with snapshot, fsync, rename, hash readback, reconciliation, and rollback.
- `cargo run --example sqlite` — optimistic-version SQLite mutation with persisted-state verification, reconciliation, and rollback.
- `cargo run --example unreliable_api` — adversarial fake remote API covering timeout-before-effect, lost-response-after-effect, duplicate requests, partial external writes, conflicting state, reconciliation, verification, and rollback.

## Tests

`cargo test --locked --all-targets --all-features` covers:

- lifecycle and state-transition legality;
- K1–K7 safety invariants;
- duplicate execution IDs and duplicate idempotency keys;
- persistent claim-store restart behavior;
- unknown outcomes and no-blind-retry semantics;
- crashes at all eight fault points;
- restart recovery from `Prepared`, `Committed`, `RollbackPending`, and `Verified`;
- verification failure, rollback failure, partial rollback failure, and non-compensable actions;
- journal/evidence/idempotency infrastructure failure boundaries;
- evidence tamper detection and secret redaction;
- property/model-generated lifecycle and fault cases;
- the adversarial fake remote API modes.

## Benchmarks

`benches/kernel.rs` separates the three costs requested by the build brief:

```bash
cargo bench --locked --bench kernel
```

It benchmarks core verified success, core verification-failure → verified rollback, and a durable fsync-backed success path using `FileJournal`, `JsonlEvidenceStore`, and `FileIdempotencyStore`. The harness emits explicit median/p95 samples in addition to Criterion's statistical report.

### Reference benchmark evidence

The reference run was executed on GitHub Actions `ubuntu-24.04`, Rust stable 1.99.0, on 2026-10-08. These are **environment-specific observations**, not universal latency guarantees.

| Path | Samples | Median | p95 |
|---|---:|---:|---:|
| core verified success | 512 | 7.013 µs | 10.910 µs |
| verification failure → verified rollback | 512 | 9.117 µs | 15.729 µs |
| durable verified success with fsync | 64 | 1.990 ms | 2.522 ms |

Criterion from the same run reported approximately `7.23–7.32 µs`, `9.45–9.65 µs`, and `2.29–2.42 ms` respectively. The complete validation and benchmark log is preserved by the linked [GitHub Actions run #75](https://github.com/SaridakisStamatisChristos/Execution-Assurance-Microkernel/actions/runs/37857640762).

CI compiles the benchmark target on every change; benchmark execution is intentionally separate from the deterministic correctness gate because shared-runner latency is noisy.

## Scope

This is deliberately **not** an agent framework, LLM system, workflow engine, distributed transaction coordinator, authentication layer, message broker, cloud platform, vector database, plugin system, or UI.

Its claim is intentionally narrow:

> **A minimal reference implementation showing how externally effectful software can distinguish attempted execution from verified execution.**

See [`docs/SPEC.md`](docs/SPEC.md), [`docs/STATE_MACHINE.md`](docs/STATE_MACHINE.md), and [`docs/FAILURE_MODEL.md`](docs/FAILURE_MODEL.md).
