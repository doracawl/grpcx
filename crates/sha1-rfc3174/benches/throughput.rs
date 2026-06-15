#![allow(missing_docs)]
//! Throughput bench: ours vs the community `sha1` crate at
//! 1 KiB / 64 KiB / 1 MiB input sizes. Runs under
//! `cargo bench -p sha1-rfc3174`.
//!
//! Numbers are recorded into the crate's BUDGETS.md.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use sha1::Digest as _;

const SIZES: &[usize] = &[1024, 64 * 1024, 1024 * 1024];

fn bench_sha1(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha1");
    for &n in SIZES {
        let buf = vec![0x5Au8; n];
        group.throughput(Throughput::Bytes(n as u64));
        group.bench_function(format!("ours/{n}"), |b| {
            b.iter(|| sha1_rfc3174::sha1(black_box(&buf)))
        });
        group.bench_function(format!("community-sha1/{n}"), |b| {
            b.iter(|| {
                let mut h = sha1::Sha1::new();
                h.update(black_box(&buf));
                h.finalize()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_sha1);
criterion_main!(benches);
