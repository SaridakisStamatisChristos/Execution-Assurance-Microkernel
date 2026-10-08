use criterion::{black_box, criterion_group, criterion_main, Criterion};
use execution_assurance_microkernel::{Action, CheckRecord, CommitStatus, Kernel};
use std::convert::Infallible;

#[derive(Debug)]
struct Increment;

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

    fn commit(&self, ctx: &mut u64) -> CommitStatus<Self::Output, Self::Error> {
        *ctx += 1;
        CommitStatus::Confirmed(*ctx)
    }

    fn verify(&self, ctx: &u64, output: &u64) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if ctx == output {
            CheckRecord::pass("readback", ctx.to_string())
        } else {
            CheckRecord::fail("readback", "mismatch")
        }])
    }

    fn rollback(&self, ctx: &mut u64, snapshot: &u64) -> Result<(), Self::Error> {
        *ctx = *snapshot;
        Ok(())
    }

    fn verify_rollback(&self, ctx: &u64, snapshot: &u64) -> Result<Vec<CheckRecord>, Self::Error> {
        Ok(vec![if ctx == snapshot {
            CheckRecord::pass("rollback", "restored")
        } else {
            CheckRecord::fail("rollback", "mismatch")
        }])
    }
}

fn benchmark_kernel(c: &mut Criterion) {
    c.bench_function("verified_increment", |b| {
        b.iter(|| {
            let mut world = 0_u64;
            let result = Kernel::default().execute(Increment, &mut world).unwrap();
            black_box(result);
        });
    });
}

criterion_group!(benches, benchmark_kernel);
criterion_main!(benches);
