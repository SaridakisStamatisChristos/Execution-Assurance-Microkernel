# Recovery Trust and Local Durability Hardening

This note documents the residual hardening applied after the K1-K11 execution-semantics work. It narrows local recovery, evidence-trust, and filesystem-durability failure surfaces without changing the kernel's declared scope or introducing distributed coordination.

## 1. Process-local recovery ownership

`Kernel::recover(execution_id, ...)` acquires a process-local guard for the execution ID before reading evidence, reading the journal, or invoking any action recovery hook.

If another recovery for the same execution ID is already active in the same process, the contender fails closed with:

```text
ExecutionError::RecoveryInProgress(execution_id)
```

The guard is RAII-managed and released on every ordinary return or unwind path.

This is intentionally **not** a distributed lease or fencing protocol. Multiple processes or hosts that can recover the same durable execution still require external ownership coordination appropriate to that deployment.

## 2. Evidence integrity before recovery trust

Recovery does not treat a located evidence record as authoritative solely because it exists.

Before terminal evidence can cause `AlreadyFinalized`, or before a nonterminal checkpoint can inform recovery, the kernel verifies the record's SHA-256 seal with `ExecutionRecord::verify_hash()`.

- valid seal: recovery may continue;
- missing or invalid seal: fail closed with `ExecutionError::EvidenceIntegrity`;
- seal verification/serialization error: fail closed with `ExecutionError::EvidenceIntegrity`.

The record seal remains a mutation-detection mechanism, **not** a cryptographic signature or trust anchor.

### Legacy schema compatibility

Schema 3 predates `RollbackRecord.verification_indeterminate`; schema 2 predates `VerificationRecord.indeterminate`. Both fields are serde-defaulted when older records are decoded.

Hash verification is schema-aware so a legitimate older sealed record is checked against its historical serialized shape rather than against bytes that include fields introduced by a later schema. Regression tests cover schema-2 and schema-3 records with those fields omitted.

## 3. Atomic-file namespace durability

The atomic-file reference action fsyncs temporary file contents before rename and, on Unix, requires a parent-directory `sync_all()` before a newly written namespace entry may be considered durably committed.

The parent-directory sync is deliberately platform-scoped:

- Unix: the parent directory is opened and `sync_all()` is invoked;
- non-Unix: no equivalent portable directory-fsync guarantee is claimed by this reference action.

### Post-rename sync failure remains commit uncertainty

A parent-directory sync can fail **after** rename has already made the replacement visible. The commit path therefore does not misclassify that condition as a known failed commit:

```text
rename succeeds
parent-directory sync fails
=> CommitStatus::Unknown
=> reconciliation before any success claim or retry decision
```

Reconciliation now closes the remaining ambiguity instead of trusting visibility alone. If the expected replacement is visible, reconciliation attempts the parent-directory sync again. Only a successful durability sync permits `ReconciliationResult::Committed`; another sync failure returns `ReconciliationResult::Unresolved`.

```text
expected bytes visible
AND parent-directory sync succeeds
=> ReconciliationResult::Committed

expected bytes visible
AND parent-directory sync still fails
=> ReconciliationResult::Unresolved
```

This prevents a visible-but-not-yet-durable directory entry from being promoted to an established commit merely because readback succeeded.

### Rollback durability is verified, not assumed

Compensation separates the namespace mutation from the durability proof:

1. `rollback_status` restores or removes the target namespace entry;
2. `verify_rollback` independently verifies the restored state;
3. when the rollback state matches the snapshot, `verify_rollback` fsyncs the parent directory before reporting a passing rollback check.

Therefore a parent-directory sync failure after a visible rollback does **not** become immediate terminal rollback failure. It surfaces through the existing rollback-verification uncertainty path:

```text
rollback mutation visible
parent-directory durability check fails
=> verify_rollback returns observer/durability error
=> RollbackVerificationRequired
=> durable head remains RollbackPending
=> recovery verifies again before considering another compensation attempt
```

A later successful rollback verification both observes the expected snapshot and establishes parent-directory durability before the execution can finalize as rolled back.

## 4. Regression coverage

`tests/recovery_hardening.rs` proves that:

- simultaneous in-process recovery of the same execution fails closed for the contender;
- tampered terminal evidence is rejected before it can be trusted;
- schema-3 evidence remains hash-verifiable after omission of its later rollback-indeterminacy field;
- schema-2 evidence remains hash-verifiable after omission of its later verification-indeterminacy field.

The atomic-file example additionally tests:

- conflict-aware compensation preserves unrelated external content;
- owned content can be restored;
- removal compensation mutates the namespace correctly;
- failure of parent-directory sync after rename is classified as `CommitStatus::Unknown`;
- visible replacement bytes do not reconcile to `Committed` while parent-directory durability remains unestablished;
- reconciliation can later establish the commit after a successful durability sync;
- rollback-state visibility plus failed parent-directory sync remains verification uncertainty;
- a later successful rollback durability verification passes without repeating the namespace mutation.

## 5. Scope boundary

These changes strengthen the local reference implementation. They do not claim:

- cross-process or distributed recovery fencing;
- distributed exactly-once execution;
- cryptographic evidence authenticity/non-repudiation;
- portable parent-directory durability semantics on every operating system;
- encrypted persistence.

Those remain deployment- or adapter-specific responsibilities.
