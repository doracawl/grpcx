#![allow(missing_docs)]
//! Throughput bench: ours vs the community `base64` crate
//! for both encode and decode at 1 KiB / 64 KiB / 1 MiB.

use base64::Engine as _;
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

const SIZES: &[usize] = &[1024, 64 * 1024, 1024 * 1024];

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("base64-encode");
    for &n in SIZES {
        let buf = vec![0xA5u8; n];
        group.throughput(Throughput::Bytes(n as u64));
        group.bench_function(format!("ours/{n}"), |b| {
            b.iter(|| base64_rfc4648::encode(black_box(&buf)))
        });
        group.bench_function(format!("community-base64/{n}"), |b| {
            b.iter(|| base64::engine::general_purpose::STANDARD.encode(black_box(&buf)))
        });
    }
    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("base64-decode");
    for &n in SIZES {
        let buf = vec![0xA5u8; n];
        let encoded = base64_rfc4648::encode(&buf);
        group.throughput(Throughput::Bytes(n as u64));
        group.bench_function(format!("ours/{n}"), |b| {
            b.iter(|| base64_rfc4648::decode(black_box(&encoded)).unwrap())
        });
        group.bench_function(format!("community-base64/{n}"), |b| {
            b.iter(|| {
                base64::engine::general_purpose::STANDARD
                    .decode(black_box(&encoded))
                    .unwrap()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode);
criterion_main!(benches);
