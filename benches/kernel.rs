use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use execution_assurance_microkernel::{
    Action, CheckRecord, Clock, CommitStatus, EffectPermit, EvidenceStore, FileIdempotencyStore,
    FileJournal, IdGenerator, IdempotencyStore, JsonlEvidenceStore, Kernel, NoFaultInjector,
    SequenceIdGenerator, SystemClock,
};
use std::{convert::Infallible, fs, sync::Arc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
struct Increment {
    verify_ok: bool,
}

impl Action for Increment {
    type Context = u64;
    type Output = u64;
    type Error = Infallible;
    type Snapshot = u64;

    fn action_id(&self) -> String {
        "bench-increment".to_string()
    }

    fn action_type(&self) -> &'static str {
        "bench_increment"
    }

    fn validate(&self, _ctx: &u64) -> Result<(), Self::Error> {
        Ok(())
    }

    fn snapshot(&self, ctx: &u64) -> Result<Self::Snapshot, Self::Error> {
        Ok(*ctx)
    }

    fn commit(
        &self,
        _permit: &EffectPermit,
        ctx: &mut u64,
    ) -> CommitStatus<Self::Output, Self::Error> {
        *ctx += 1;
        CommitStatus::Confirmed(*ctx)
    }

    fn verify(&self, ctx: &u64, output: &u64) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if self.verify_ok && ctx == output {
            CheckRecord::pass("readback", ctx.to_string())
        } else {
            CheckRecord::fail("readback", "forced verification failure")
        }])
    }

    fn rollback(
        &self,
        _permit: &EffectPermit,
        ctx: &mut u64,
        snapshot: &u64,
    ) -> Result<(), Self::Error> {
        *ctx = *snapshot;
        Ok(())
    }

    fn verify_rollback(
        &self,
        ctx: &u64,
        snapshot: &u64,
    ) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if ctx == snapshot {
            CheckRecord::pass("rollback", "restored")
        } else {
            CheckRecord::fail("rollback", "mismatch")
        }])
    }
}

fn durable_kernel(root: &std::path::Path) -> Kernel {
    let journal: Arc<dyn execution_assurance_microkernel::Journal> =
        Arc::new(FileJournal::open(root.join("journal.jsonl")).unwrap());
    let evidence: Arc<dyn EvidenceStore> =
        Arc::new(JsonlEvidenceStore::open(root.join("evidence.jsonl")).unwrap());
    let idempotency: Arc<dyn IdempotencyStore> =
        Arc::new(FileIdempotencyStore::open(root.join("claims")).unwrap());
    let faults = Arc::new(NoFaultInjector);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(SequenceIdGenerator::starting_at(1));
    Kernel::with_components(journal, evidence, idempotency, faults, clock, ids)
}

fn benchmark_kernel(c: &mut Criterion) {
    c.bench_function("core/success_verified", |b| {
        let kernel = Kernel::default();
        b.iter(|| {
            let mut world = 0_u64;
            black_box(
                kernel
                    .execute(Increment { verify_ok: true }, &mut world)
                    .unwrap(),
            );
        });
    });

    c.bench_function("core/verification_failure_verified_rollback", |b| {
        let kernel = Kernel::default();
        b.iter(|| {
            let mut world = 0_u64;
            black_box(
                kernel
                    .execute(Increment { verify_ok: false }, &mut world)
                    .unwrap(),
            );
        });
    });

    c.bench_function("durable/success_verified_fsync", |b| {
        b.iter_batched(
            || {
                let root = std::env::temp_dir().join(format!("eamk-bench-{}", Uuid::new_v4()));
                fs::create_dir_all(&root).unwrap();
                let kernel = durable_kernel(&root);
                (root, kernel, 0_u64)
            },
            |(root, kernel, mut world)| {
                let result = kernel
                    .execute(Increment { verify_ok: true }, &mut world)
                    .unwrap();
                black_box(result);
                fs::remove_dir_all(root).unwrap();
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group!(benches, benchmark_kernel);
criterion_main!(benches);
