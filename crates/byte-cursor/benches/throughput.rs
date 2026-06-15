#![allow(missing_docs)]
//! Throughput bench: how fast can the cursor walk a 1 MiB
//! buffer (advance-only vs take_while)? No community
//! comparator — this is the "drive a parser inner loop"
//! reference rate.

use byte_cursor::Cursor;
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

const N: usize = 1024 * 1024;

fn make_buffer() -> Vec<u8> {
    // Mostly token chars (a-z) with one newline every 80
    // bytes, so the line/col tracking gets exercised.
    (0..N)
        .map(|i| {
            if i.is_multiple_of(80) {
                b'\n'
            } else {
                b'a' + (i % 26) as u8
            }
        })
        .collect()
}

fn bench_advance(c: &mut Criterion) {
    let buf = make_buffer();
    let mut group = c.benchmark_group("byte-cursor");
    group.throughput(Throughput::Bytes(N as u64));

    group.bench_function("advance", |b| {
        b.iter(|| {
            let mut cur = Cursor::new(black_box(&buf));
            while cur.advance().is_some() {}
            cur.pos()
        })
    });

    group.bench_function("take_while", |b| {
        b.iter(|| {
            let mut cur = Cursor::new(black_box(&buf));
            // Reads runs of letters, then skips one newline,
            // and loops. Models the lexer "read_ident +
            // skip_trivia" inner loop.
            while !cur.at_end() {
                cur.take_while(|b| b.is_ascii_alphabetic());
                cur.advance();
            }
            cur.pos()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_advance);
criterion_main!(benches);
