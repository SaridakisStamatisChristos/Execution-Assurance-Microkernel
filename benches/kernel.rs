use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use execution_assurance_microkernel::{
    Action, CheckRecord, Clock, CommitStatus, EffectPermit, EvidenceStore, FileIdempotencyStore,
    FileJournal, IdGenerator, IdempotencyStore, JsonlEvidenceStore, Kernel, NoFaultInjector,
    SequenceIdGenerator, SystemClock,
};
use std::{
    convert::Infallible,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
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

fn durable_kernel(root: &Path) -> Kernel {
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

struct DurableCase {
    root: PathBuf,
    kernel: Kernel,
    world: u64,
}

impl DurableCase {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("eamk-bench-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let kernel = durable_kernel(&root);
        Self {
            root,
            kernel,
            world: 0,
        }
    }
}

impl Drop for DurableCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn percentile_index(len: usize, numerator: usize, denominator: usize) -> usize {
    len.saturating_mul(numerator)
        .div_ceil(denominator)
        .saturating_sub(1)
        .min(len.saturating_sub(1))
}

fn summarize(label: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    let median = samples[percentile_index(samples.len(), 50, 100)];
    let p95 = samples[percentile_index(samples.len(), 95, 100)];
    println!(
        "EAMK_PERCENTILES name={label} samples={} median_ns={} p95_ns={}",
        samples.len(),
        median.as_nanos(),
        p95.as_nanos()
    );
}

fn emit_percentile_report() {
    const CORE_SAMPLES: usize = 512;
    const DURABLE_SAMPLES: usize = 64;

    let kernel = Kernel::default();
    let mut success = Vec::with_capacity(CORE_SAMPLES);
    for _ in 0..CORE_SAMPLES {
        let mut world = 0_u64;
        let start = Instant::now();
        black_box(
            kernel
                .execute(Increment { verify_ok: true }, &mut world)
                .unwrap(),
        );
        success.push(start.elapsed());
    }
    summarize("core_success_verified", success);

    let kernel = Kernel::default();
    let mut rollback = Vec::with_capacity(CORE_SAMPLES);
    for _ in 0..CORE_SAMPLES {
        let mut world = 0_u64;
        let start = Instant::now();
        black_box(
            kernel
                .execute(Increment { verify_ok: false }, &mut world)
                .unwrap(),
        );
        rollback.push(start.elapsed());
    }
    summarize("core_verification_failure_verified_rollback", rollback);

    let mut durable = Vec::with_capacity(DURABLE_SAMPLES);
    for _ in 0..DURABLE_SAMPLES {
        let mut case = DurableCase::new();
        let start = Instant::now();
        black_box(
            case.kernel
                .execute(Increment { verify_ok: true }, &mut case.world)
                .unwrap(),
        );
        durable.push(start.elapsed());
    }
    summarize("durable_success_verified_fsync", durable);
}

fn benchmark_kernel(c: &mut Criterion) {
    emit_percentile_report();

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
            DurableCase::new,
            |mut case| {
                black_box(
                    case.kernel
                        .execute(Increment { verify_ok: true }, &mut case.world)
                        .unwrap(),
                );
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group!(benches, benchmark_kernel);
criterion_main!(benches);
