# Execution Assurance Microkernel

[![CI](https://github.com/SaridakisStamatisChristos/Execution-Assurance-Microkernel/actions/workflows/ci.yml/badge.svg)](https://github.com/SaridakisStamatisChristos/Execution-Assurance-Microkernel/actions/workflows/ci.yml)

A compact Rust reference implementation for one idea:

> **Commit is not success. Success is a confirmed commit plus independently verified postconditions.**

The microkernel owns the lifecycle of an externally effectful action:

```text
PROPOSE -> VALIDATE -> PREPARE -> COMMIT -> VERIFY -> RECORD
                                      |         |
                                      |         +-- failure --> ROLLBACK -> VERIFY ROLLBACK
                                      +-- unknown --> RECONCILE (never blind retry)
```

It does **not** decide what action to take. It decides whether a proposed action may execute, whether it actually succeeded, what to do when the outcome is uncertain, and what evidence exists afterward.

## Properties

- explicit state machine; invalid transitions are rejected;
- preconditions emit inspectable evidence rather than booleans;
- rollback snapshot captured before side effects;
- invariants checked before commit, after commit, and after rollback;
- successful transport/return value is insufficient without postcondition verification;
- rollback is independently verified and rollback failure is explicit;
- fail-closed idempotency keys prevent duplicate effects;
- unknown commit outcomes reconcile before any retry decision;
- fsync-backed write-ahead journal for crash classification;
- immutable, SHA-256-sealed execution records;
- deterministic fault injection and recovery-plan tests.

## Tiny API

```rust
let result = Kernel::default().execute(action, &mut context)?;
```

For effect deduplication:

```rust
let result = kernel.execute_idempotent(
    action,
    &mut context,
    IdempotencyKey::from("request-123"),
)?;
```

Application code implements `Action`; it should not call its own `commit` as an execution path. The kernel sequences validation, snapshot, durable `Prepared`, commit, verification, rollback/reconciliation when necessary, and evidence persistence.

## Failure semantics

| Situation | Kernel outcome |
|---|---|
| validation/precondition fails | `Rejected` |
| snapshot/pre-commit invariant fails | `Aborted` |
| commit known to fail | `CommitFailed` |
| commit response is lost | reconciliation required before retry |
| postcondition/invariant fails, rollback verifies | `RolledBack` |
| rollback or rollback verification fails | `RollbackFailed` |
| commit + postconditions verify | `Success` |

`ExecutionOutcome::Success` is therefore stronger than “the call returned `Ok`.”

## Crash safety

The journal records state changes using append + flush + `sync_data()`. In particular, `Prepared` is durably recorded before the effect. If the process dies after the effect but before `Committed` can be written, recovery sees `Prepared` and requires reconciliation before retry. A `Committed` journal head requires verification or rollback; `RollbackPending` requires completing compensation.

The recovery module intentionally returns **recovery directives**, not magical replay. Rehydrating application-specific actions belongs outside this kernel.

## Reference examples

- `cargo run --example atomic_file` — atomic file replacement with snapshot, fsync, rename, readback hash, and rollback.
- `cargo run --example sqlite` — optimistic-version SQLite mutation with persisted-state verification.
- `cargo run --example unreliable_api` — lost response after server-side commit, followed by reconciliation and readback verification.

## Test strategy

`cargo test --all-targets` covers lifecycle rules, duplicate idempotency keys, unknown outcomes, journal recovery classification, state-machine legality, model/property checks, and all kernel fault points.

## Scope

This is deliberately **not** an agent framework, workflow engine, distributed transaction coordinator, auth layer, message broker, cloud platform, or UI. Its claim is intentionally narrow:

> A minimal reference implementation showing how externally effectful software can distinguish attempted execution from verified execution.

See [`docs/SPEC.md`](docs/SPEC.md), [`docs/STATE_MACHINE.md`](docs/STATE_MACHINE.md), and [`docs/FAILURE_MODEL.md`](docs/FAILURE_MODEL.md).
