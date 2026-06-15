//! Cross-language comparator — Rust (ours + community
//! crates), sharing one timing methodology with the C and
//! Go harnesses: per (op, size): ~100 ms warmup, then 5
//! batches of >= 10 ms each; report the best (minimum)
//! ns/op.
//!
//! Output: CSV lines  `impl,op,size,ns_per_op,gib_s`

use base64::Engine as _;
use sha1::Digest as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

fn run_one(impl_name: &str, op_name: &str, mut fn_once: impl FnMut() -> u8, reported_size: usize) {
    // Warmup ~100 ms.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(100) {
        black_box(fn_once());
    }

    // Pick K so one batch is >= ~10 ms.
    let t0 = Instant::now();
    black_box(fn_once());
    let once = t0.elapsed().as_nanos().max(1) as f64;
    let k = (1e7 / once) as usize + 1;

    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let t0 = Instant::now();
        for _ in 0..k {
            black_box(fn_once());
        }
        let per = t0.elapsed().as_nanos() as f64 / k as f64;
        if per < best {
            best = per;
        }
    }

    let gib_s = (reported_size as f64 / 1_073_741_824.0) / (best / 1e9);
    println!("{impl_name},{op_name},{reported_size},{best:.1},{gib_s:.4}");
}

fn main() {
    for n in [1024usize, 65536, 1048576] {
        let buf: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
        let encoded = base64_rfc4648::encode(&buf);

        // ---- ours ----
        run_one("ours-rust", "sha1", || sha1_rfc3174::sha1(&buf)[0], n);
        run_one(
            "ours-rust",
            "b64-encode",
            || base64_rfc4648::encode(&buf).as_bytes()[0],
            n,
        );
        run_one(
            "ours-rust",
            "b64-decode",
            || base64_rfc4648::decode(&encoded).unwrap()[0],
            n,
        );

        // ---- community crates ----
        run_one(
            "community-rust",
            "sha1",
            || {
                let mut h = sha1::Sha1::new();
                h.update(&buf);
                h.finalize()[0]
            },
            n,
        );
        run_one(
            "community-rust",
            "b64-encode",
            || {
                base64::engine::general_purpose::STANDARD
                    .encode(&buf)
                    .as_bytes()[0]
            },
            n,
        );
        run_one(
            "community-rust",
            "b64-decode",
            || {
                base64::engine::general_purpose::STANDARD
                    .decode(&encoded)
                    .unwrap()[0]
            },
            n,
        );
    }
}
