#![allow(missing_docs)]
//! Throughput bench: parse a realistic
//! `Sec-WebSocket-Extensions`-style header value through
//! `read_token` + `read_quoted_string` + `token_list_contains`.
//! No community comparator at this granularity — `httparse`
//! does full HTTP/1 messages, not isolated header values.

use byte_cursor::Cursor;
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use http1_syntax::{read_quoted_string, read_token, token_list_contains};

const EXT_HEADER: &str = "permessage-deflate; client_max_window_bits=\"15\"; \
server_max_window_bits=15; client_no_context_takeover, \
custom-ext; level=\"high speed\"; mode=balanced";

const TOKEN_LIST: &str = "gzip, deflate, br, zstd, identity";

fn bench_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("http1-syntax");

    group.throughput(Throughput::Bytes(EXT_HEADER.len() as u64));
    group.bench_function("read_token_x4_plus_quoted", |b| {
        b.iter(|| {
            let mut cur = Cursor::from_str(black_box(EXT_HEADER));
            let mut total = 0;
            // Mix of token reads + quoted-string reads to
            // exercise both code paths.
            for _ in 0..4 {
                total += read_token(&mut cur).len();
                if cur.try_consume(b'"') {
                    cur.advance();
                }
            }
            cur = Cursor::from_str(black_box(EXT_HEADER));
            // Find the first quoted bit and parse it.
            while let Some(b) = cur.peek() {
                if b == b'"' {
                    break;
                }
                cur.advance();
            }
            if !cur.at_end() {
                total += read_quoted_string(&mut cur).map(|s| s.len()).unwrap_or(0);
            }
            total
        })
    });

    group.throughput(Throughput::Bytes(TOKEN_LIST.len() as u64));
    group.bench_function("token_list_contains_hit", |b| {
        b.iter(|| token_list_contains(black_box(TOKEN_LIST), black_box("zstd")))
    });
    group.bench_function("token_list_contains_miss", |b| {
        b.iter(|| token_list_contains(black_box(TOKEN_LIST), black_box("xz")))
    });

    group.finish();
}

criterion_group!(benches, bench_parsing);
criterion_main!(benches);
