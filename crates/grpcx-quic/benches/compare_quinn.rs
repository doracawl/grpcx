//! Head-to-head bench: `grpcx-quic`'s wire-format codec vs
//! `quinn-proto`'s wire-format primitives.
//!
//! Scope intentionally narrow: we bench the codec layer only —
//! varint encode / decode. Connection-state-machine benches and
//! end-to-end one-RTT throughput defer to the v1.x perf pass
//! (matches the deferral on L1 / L2 / L3 `compare_*` harnesses).
//!
//! Fairness invariants:
//!
//! - Same iteration counts on both sides.
//! - The encode loop runs the shortest valid form on both sides
//!   (the QUIC varint encoding is not unique; both codecs pick
//!   the shortest).
//! - Buffers are reset per iter; allocation is hoisted out of the
//!   measured region.
//! - No `criterion::black_box` on quinn-proto side except where
//!   also applied on the grpcx-quic side.
//!
//! Numbers publish in the v1.x perf pass (see
//! `crates/grpcx-quic/BUDGETS.md`).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use grpcx_quic::{frame_varint_len, varint_field_len};

// ====================================================================
// Varint: shortest-form length classifier.
// ====================================================================

const VARINT_TEST_VALUES: &[u64] = &[
    0,
    1,
    63,
    64,
    16_383,
    16_384,
    (1u64 << 30) - 1,
    1u64 << 30,
    (1u64 << 62) - 1,
];

fn bench_varint_len(c: &mut Criterion) {
    let mut group = c.benchmark_group("varint/len_classifier");

    group.bench_function("grpcx_quic_frame_varint_len", |b| {
        b.iter(|| {
            let mut acc: usize = 0;
            for &v in VARINT_TEST_VALUES {
                acc = acc.wrapping_add(frame_varint_len(black_box(v)));
            }
            acc
        });
    });

    group.bench_function("grpcx_quic_varint_field_len", |b| {
        b.iter(|| {
            let mut acc: usize = 0;
            for &v in VARINT_TEST_VALUES {
                acc = acc.wrapping_add(varint_field_len(black_box(v)));
            }
            acc
        });
    });

    // quinn-proto's `VarInt::size()` is `pub(crate)` so we cannot
    // call it from outside. We bench the closest public surface —
    // `VarInt::from_u64` construction (which internally does the
    // shortest-form bounds check). Numbers aren't directly
    // comparable to grpcx-quic's classifiers; the v1.x perf pass
    // will land a like-for-like comparison once we have an
    // encode-into-buffer bench (which both codecs do expose).
    group.bench_function("quinn_proto_varint_from_u64", |b| {
        b.iter(|| {
            let mut acc: u64 = 0;
            for &v in VARINT_TEST_VALUES {
                let vi = quinn_proto::VarInt::from_u64(black_box(v)).expect("in range");
                acc = acc.wrapping_add(vi.into_inner());
            }
            acc
        });
    });

    group.finish();
}

criterion_group!(benches, bench_varint_len);
criterion_main!(benches);
