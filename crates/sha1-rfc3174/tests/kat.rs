//! RFC 3174 § 7 test vectors + incremental-API consistency
//! + WebSocket reference vector.

use sha1_rfc3174::{Sha1, sha1, sha1_hex};

#[test]
fn hw_matches_scalar_differential() {
    // Differential test: the dispatched path (hardware on
    // aarch64+sha2, scalar elsewhere) must agree with the
    // scalar oracle byte-for-byte at every length 0..=600
    // — covers 0..9 blocks plus every tail size, both
    // padding shapes (1-block and 2-block finalize), and
    // the multi-block register-resident loop.
    let data: Vec<u8> = (0..600usize)
        .map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8)
        .collect();
    for n in 0..=600usize {
        assert_eq!(
            sha1(&data[..n]),
            sha1_rfc3174::sha1_scalar_oracle(&data[..n]),
            "hw/scalar divergence at length {n}"
        );
    }
}

#[test]
fn incremental_partial_buffer_fills_to_64_then_flushes() {
    // Feed in 2-byte chunks summing to exactly 64 bytes —
    // forces the "partial buffer fills via copy_from_slice
    // and triggers process_block" branch that the
    // existing fast-path tests don't exercise.
    let data: Vec<u8> = (0..64u8).collect();
    let mut h = Sha1::new();
    for chunk in data.chunks(2) {
        h.update(chunk);
    }
    assert_eq!(h.finalize(), sha1(&data));
}

// ============================================================
// RFC 3174 § 7 — Sample Outputs.
// ============================================================

#[test]
fn rfc3174_test_1_abc() {
    // RFC 3174 § 7.3 Test 1: "abc"
    // → A9993E36 4706816A BA3E2571 7850C26C 9CD0D89D
    assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
}

#[test]
fn rfc3174_test_2_56_char() {
    // RFC 3174 § 7.3 Test 2: 56 chars (block-spanning padding edge).
    let input = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    assert_eq!(sha1_hex(input), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
}

#[test]
fn rfc3174_test_3_million_a() {
    // RFC 3174 § 7.3 Test 3: 1,000,000 `a` bytes — the
    // long-input lock. Verifies incremental block
    // processing across many calls.
    let mut hasher = Sha1::new();
    let chunk = [b'a'; 1024];
    for _ in 0..976 {
        hasher.update(&chunk);
    }
    // 976 * 1024 = 999,424. Need 576 more.
    hasher.update(&[b'a'; 576]);
    let digest = hasher.finalize();
    assert_eq!(hex_str(&digest), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
}

#[test]
fn rfc3174_test_4_block_aligned() {
    // RFC 3174 § 7.3 Test 4: "01234567" repeated 80 times
    // = 640 bytes (the macro is 64 bytes, fed 10 times by
    // the reference C driver). Block-aligned; checks
    // multi-block state propagation across 10 blocks.
    let input: Vec<u8> = "01234567".repeat(80).into_bytes();
    assert_eq!(sha1_hex(&input), "dea356a2cddd90c7a7ecedc5ebb563934f460452");
}

// ============================================================
// Empty input + 1-byte input edge cases.
// ============================================================

#[test]
fn empty_input() {
    assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
}

#[test]
fn single_byte() {
    assert_eq!(sha1_hex(b"\0"), "5ba93c9db0cff93f52b521d7420e43f6eda2784f");
}

#[test]
fn quick_brown_fox() {
    let input = b"The quick brown fox jumps over the lazy dog";
    assert_eq!(sha1_hex(input), "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12");
}

// ============================================================
// Incremental API consistency with one-shot.
// ============================================================

#[test]
fn incremental_matches_one_shot() {
    let input = b"The quick brown fox jumps over the lazy dog";
    let one_shot = sha1(input);
    let mut hasher = Sha1::new();
    hasher.update(input);
    let incremental = hasher.finalize();
    assert_eq!(one_shot, incremental);
}

#[test]
fn incremental_arbitrary_chunking() {
    // Hash "The quick brown fox jumps over the lazy dog"
    // in chunks of 1, 7, 13, then rest. Result must match
    // one-shot.
    let input = b"The quick brown fox jumps over the lazy dog";
    let one_shot = sha1(input);
    let mut hasher = Sha1::new();
    hasher.update(&input[..1]);
    hasher.update(&input[1..8]);
    hasher.update(&input[8..21]);
    hasher.update(&input[21..]);
    assert_eq!(hasher.finalize(), one_shot);
}

#[test]
fn incremental_byte_by_byte() {
    let input = b"Lorem ipsum dolor sit amet";
    let one_shot = sha1(input);
    let mut hasher = Sha1::new();
    for byte in input {
        hasher.update(&[*byte]);
    }
    assert_eq!(hasher.finalize(), one_shot);
}

#[test]
fn incremental_large_chunks() {
    // Feed 200KB across multiple 32KB chunks. Verifies the
    // "fast path" through the update loop (direct
    // multi-block consumption from input slice without
    // buffering).
    let chunk_a: Vec<u8> = (0..32768).map(|i| (i % 256) as u8).collect();
    let mut full = Vec::new();
    for _ in 0..7 {
        full.extend_from_slice(&chunk_a);
    }
    let one_shot = sha1(&full);
    let mut hasher = Sha1::new();
    for _ in 0..7 {
        hasher.update(&chunk_a);
    }
    assert_eq!(hasher.finalize(), one_shot);
}

#[test]
fn default_equals_new() {
    let a = Sha1::default();
    let b = Sha1::new();
    // Both finalize on empty input → same digest.
    assert_eq!(a.finalize(), b.finalize());
}

// ============================================================
// Block-boundary edge cases.
// ============================================================

#[test]
fn input_lengths_around_block_boundary() {
    // SHA-1 buffer/length math is sensitive at lengths
    // 55 / 56 / 64 / 119 / 120 / 128 (padding straddles
    // a block at these points).
    for n in [0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129] {
        let input: Vec<u8> = (0..n).map(|i| (i % 256) as u8).collect();
        let one_shot = sha1(&input);
        let mut hasher = Sha1::new();
        hasher.update(&input);
        assert_eq!(hasher.finalize(), one_shot, "length {n}");
    }
}

// ============================================================
// WebSocket RFC 6455 § 1.3 reference input — the lift
// driver.
// ============================================================

#[test]
fn rfc6455_section_1_3_intermediate_hash() {
    let input = b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    assert_eq!(sha1_hex(input), "b37a4f2cc0624f1690f64606cf385945b2bec4ea");
}

// ============================================================
// Helper.
// ============================================================

fn hex_str(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0F) as u32, 16).unwrap());
    }
    s
}
