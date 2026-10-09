# Recovery Trust and Local Durability Hardening

This note documents the residual hardening applied after the K1-K11 execution-semantics work. It narrows three failure surfaces without changing the kernel's declared scope or introducing distributed coordination.

## 1. Process-local recovery ownership

`Kernel::recover(execution_id, ...)` now acquires a process-local guard for the execution ID before reading evidence, reading the journal, or invoking any action recovery hook.

If another recovery for the same execution ID is already active in the same process, the contender fails closed with:

```text
ExecutionError::RecoveryInProgress(execution_id)
```

The guard is RAII-managed and released on every ordinary return or unwind path.

This is intentionally **not** a distributed lease or fencing protocol. Multiple processes or hosts that can recover the same durable execution still require external ownership coordination appropriate to that deployment.

## 2. Evidence integrity before recovery trust

Recovery no longer treats a located evidence record as authoritative solely because it exists.

Before terminal evidence can cause `AlreadyFinalized`, or before a nonterminal checkpoint can inform recovery, the kernel verifies the record's SHA-256 seal with `ExecutionRecord::verify_hash()`.

- valid seal: recovery may continue;
- missing or invalid seal: fail closed with `ExecutionError::EvidenceIntegrity`;
- seal verification/serialization error: fail closed with `ExecutionError::EvidenceIntegrity`.

The record seal remains a mutation-detection mechanism, **not** a cryptographic signature or trust anchor.

### Legacy schema compatibility

Schema 3 predates `RollbackRecord.verification_indeterminate`; schema 2 predates `VerificationRecord.indeterminate`. Both fields are serde-defaulted when older records are decoded.

Hash verification is schema-aware so a legitimate older sealed record is checked against its historical serialized shape rather than against bytes that include fields introduced by a later schema. Regression tests cover schema-2 and schema-3 records with those fields omitted.

## 3. Atomic-file namespace durability

The atomic-file reference action already fsynced temporary file contents before rename. On Unix it now also fsyncs the parent directory after a successful rename so the namespace change is made durable before commit confirmation.

Rollback that removes a file likewise fsyncs the parent directory after successful removal.

The parent-directory sync is deliberately platform-scoped:

- Unix: parent directory opened and `sync_all()` invoked;
- non-Unix: no equivalent portable directory-fsync guarantee is claimed by this reference action.

### Post-rename sync failure is uncertainty

A parent-directory sync can fail **after** the rename has already made the replacement visible. The commit path therefore does not misclassify that condition as a known failed commit.

Instead:

```text
rename succeeds
parent-directory sync fails
=> CommitStatus::Unknown
=> reconciliation/readback before any retry decision
```

This preserves the kernel's central uncertainty rule: an error reported after an externally visible effect is not proof that the effect did not happen.

## 4. Regression coverage

`tests/recovery_hardening.rs` proves that:

- simultaneous in-process recovery of the same execution fails closed for the contender;
- tampered terminal evidence is rejected before it can be trusted;
- schema-3 evidence remains hash-verifiable after omission of its later rollback-indeterminacy field;
- schema-2 evidence remains hash-verifiable after omission of its later verification-indeterminacy field.

The atomic-file example additionally tests:

- conflict-aware compensation preserves unrelated external content;
- owned content can be restored;
- removal compensation completes correctly;
- failure of parent-directory sync after rename is classified as `CommitStatus::Unknown`, while the already-visible replacement remains detectable by reconciliation.

## 5. Scope boundary

These changes strengthen the local reference implementation. They do not claim:

- cross-process or distributed recovery fencing;
- distributed exactly-once execution;
- cryptographic evidence authenticity/non-repudiation;
- portable parent-directory durability semantics on every operating system;
- encrypted persistence.

Those remain deployment- or adapter-specific responsibilities.