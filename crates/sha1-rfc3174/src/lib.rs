#![doc = include_str!("../README.md")]
#![no_std]

//! RFC 3174 SHA-1. Pure Rust, zero deps, `no_std + alloc`.
//! See the README for the security warning, spec citation,
//! and feature matrix.

extern crate alloc;

use alloc::string::String;

// ============================================================
// RFC 3174 § 6.1 initial hash values.
// ============================================================

const H0: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

// ============================================================
// Public API — one-shot.
// ============================================================

/// SHA-1 of `data`. Returns the 20-byte digest per
/// RFC 3174 § 6.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    // Direct one-shot path — skips the incremental
    // hasher's buffering logic entirely: one multi-block
    // compress over the full input, then one padded
    // compress for the tail. Worth ~8% at 1 KiB inputs.
    let mut state = H0;
    let full = data.len() / 64 * 64;
    if full > 0 {
        compress_blocks(&mut state, &data[..full]);
    }
    let rem = &data[full..];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut last = [0u8; 128];
    last[..rem.len()].copy_from_slice(rem);
    last[rem.len()] = 0x80;
    let total = if rem.len() < 56 {
        last[56..64].copy_from_slice(&bit_len.to_be_bytes());
        64
    } else {
        last[120..128].copy_from_slice(&bit_len.to_be_bytes());
        128
    };
    compress_blocks(&mut state, &last[..total]);

    let mut out = [0u8; 20];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// SHA-1 of `data` formatted as a lowercase 40-char hex
/// string.
pub fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1(data))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0F) as u32, 16).unwrap());
    }
    s
}

// ============================================================
// Public API — incremental.
// ============================================================

/// Streaming SHA-1 hasher. Call [`update`](Self::update)
/// any number of times with chunks of input, then
/// [`finalize`](Self::finalize) once to get the digest.
#[derive(Debug, Clone)]
pub struct Sha1 {
    state: [u32; 5],
    /// Bytes buffered for the next-to-be-processed block.
    /// Always 0..=63 bytes; when it hits 64, we process
    /// and clear.
    buffer: [u8; 64],
    /// How many bytes are valid in `buffer`.
    buffered: usize,
    /// Total length of input fed so far, in bytes (not
    /// bits — we shift in finalize). Saturating at u64 max
    /// is fine; SHA-1 is undefined past 2^64-1 bits.
    total_len: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    /// Construct a fresh hasher with the RFC 3174 § 6.1
    /// initial state.
    pub fn new() -> Self {
        Sha1 {
            state: H0,
            buffer: [0u8; 64],
            buffered: 0,
            total_len: 0,
        }
    }

    /// Feed `data` into the hasher. May be called any
    /// number of times; the output is identical to one
    /// call with the concatenated input.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        // Fill the buffer first if it has partial content.
        if self.buffered > 0 {
            let need = 64 - self.buffered;
            let take = need.min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let buf = self.buffer;
                compress_blocks(&mut self.state, &buf);
                self.buffered = 0;
            }
        }

        // Process complete blocks directly from the input
        // slice — one call, so the block compressor keeps
        // the hash state in registers across blocks.
        let full = data.len() / 64 * 64;
        if full > 0 {
            compress_blocks(&mut self.state, &data[..full]);
            data = &data[full..];
        }

        // Stash the leftover.
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    /// Finalize and return the 20-byte digest. Consumes the
    /// hasher.
    pub fn finalize(mut self) -> [u8; 20] {
        // Pad: append 0x80, zeros, then 64-bit BE length.
        let bit_len = self.total_len.wrapping_mul(8);
        let mut last = [0u8; 128];
        last[..self.buffered].copy_from_slice(&self.buffer[..self.buffered]);
        last[self.buffered] = 0x80;
        let total = if self.buffered < 56 {
            last[56..64].copy_from_slice(&bit_len.to_be_bytes());
            64
        } else {
            last[120..128].copy_from_slice(&bit_len.to_be_bytes());
            128
        };
        compress_blocks(&mut self.state, &last[..total]);

        let mut out = [0u8; 20];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

// ============================================================
// Block-compressor dispatch.
//
// Hardware path: AArch64 SHA-1 crypto extensions (the
// `sha2` target feature gates SHA1C/SHA1P/SHA1M/SHA1H/
// SHA1SU0/SHA1SU1) — enabled by default on Apple Silicon
// and ARMv8-A server targets. Falls back to the portable
// scalar implementation everywhere else.
//
// The scalar implementation stays in-tree unconditionally:
// it is the spec-readable reference, the fallback for
// targets without the crypto extensions, and the
// differential-test oracle for the hardware path.
// ============================================================

/// Compress `blocks` (length must be a multiple of 64)
/// into `state`.
fn compress_blocks(state: &mut [u32; 5], blocks: &[u8]) {
    debug_assert!(blocks.len().is_multiple_of(64));
    #[cfg(all(target_arch = "aarch64", target_feature = "sha2"))]
    {
        // SAFETY: this arm only compiles when the `sha2`
        // target feature is statically enabled for the
        // whole crate (the cfg above), so the CPU is
        // guaranteed to support the SHA-1 instructions the
        // callee requires.
        unsafe { aarch64_sha::compress_blocks(state, blocks) };
    }
    #[cfg(not(all(target_arch = "aarch64", target_feature = "sha2")))]
    {
        for block in blocks.chunks_exact(64) {
            let mut buf = [0u8; 64];
            buf.copy_from_slice(block);
            process_block(state, &buf);
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "sha2"))]
mod aarch64_sha {
    //! AArch64 SHA-1 crypto-extension block compressor.
    //!
    //! Instruction sequence follows the canonical ARM
    //! reference flow (the same quad-round schedule used by
    //! OpenSSL / Linux / Go on ARMv8): 20 quad-rounds, each
    //! one `SHA1H` + `SHA1C`/`SHA1P`/`SHA1M`, with the
    //! message schedule advanced by `SHA1SU0`/`SHA1SU1`.
    //!
    //! `unsafe` here is confined to the raw 16-byte vector
    //! loads/stores (`vld1q_u8` / `vld1q_u32` /
    //! `vst1q_u32`); pointer validity is guaranteed by the
    //! caller's slice bounds. The arithmetic intrinsics are
    //! value-based.
    //!
    //! Correctness gates: the full RFC 3174 § 7 KAT suite
    //! runs against this path on aarch64, plus a
    //! differential test (`tests/kat.rs::hw_matches_scalar_
    //! differential`) comparing against the scalar oracle
    //! across block counts 1..=8.

    use core::arch::aarch64::*;

    const K0: u32 = 0x5A827999;
    const K1: u32 = 0x6ED9EBA1;
    const K2: u32 = 0x8F1BBCDC;
    const K3: u32 = 0xCA62C1D6;

    /// Load one 64-byte block as four big-endian u32x4
    /// message words.
    ///
    /// # Safety
    /// `p` must point to at least 64 readable bytes.
    #[inline]
    #[target_feature(enable = "sha2")]
    unsafe fn load_block(p: *const u8) -> (uint32x4_t, uint32x4_t, uint32x4_t, uint32x4_t) {
        // SAFETY: caller guarantees 64 readable bytes, so
        // offsets 0/16/32/48 each have 16 readable bytes.
        unsafe {
            (
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(16)))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(32)))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(p.add(48)))),
            )
        }
    }

    #[target_feature(enable = "sha2")]
    pub(super) fn compress_blocks(state: &mut [u32; 5], blocks: &[u8]) {
        let n = blocks.len() / 64;
        if n == 0 {
            return;
        }
        let base = blocks.as_ptr();

        // SAFETY: `state` is a valid `[u32; 5]`, so reading
        // 4 u32s from its base pointer is in-bounds.
        let mut abcd: uint32x4_t = unsafe { vld1q_u32(state.as_ptr()) };
        let mut e0: u32 = state[4];

        let k0 = vdupq_n_u32(K0);
        let k1 = vdupq_n_u32(K1);
        let k2 = vdupq_n_u32(K2);
        let k3 = vdupq_n_u32(K3);

        // Software pipeline: block i+1's loads + byte
        // reversal issue in the middle of block i's rounds
        // (textually after the K0 quads), hiding load
        // latency under the serial SHA round chain — the
        // same scheduling OpenSSL's sha1-armv8 perlasm
        // uses. `m*` holds the pre-loaded next block; the
        // round schedule mutates the separate `s*` set.
        //
        // SAFETY: `i == 0` and `n >= 1`, so `base` has 64
        // readable bytes.
        let (mut m0, mut m1, mut m2, mut m3) = unsafe { load_block(base) };

        for i in 0..n {
            let abcd_saved = abcd;
            let e0_saved = e0;

            let mut msg0 = m0;
            let mut msg1 = m1;
            let mut msg2 = m2;
            let mut msg3 = m3;

            let mut tmp0 = vaddq_u32(msg0, k0);
            let mut tmp1 = vaddq_u32(msg1, k0);
            let mut e1: u32;

            // Rounds 0-3 (Ch, K0)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1cq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg2, k0);
            msg0 = vsha1su0q_u32(msg0, msg1, msg2);

            // Rounds 4-7 (Ch, K0)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1cq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg3, k0);
            msg0 = vsha1su1q_u32(msg0, msg3);
            msg1 = vsha1su0q_u32(msg1, msg2, msg3);

            // Rounds 8-11 (Ch, K0)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1cq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg0, k0);
            msg1 = vsha1su1q_u32(msg1, msg0);
            msg2 = vsha1su0q_u32(msg2, msg3, msg0);

            // Rounds 12-15 (Ch, K0 → schedule crosses into K1)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1cq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg1, k1);
            msg2 = vsha1su1q_u32(msg2, msg1);
            msg3 = vsha1su0q_u32(msg3, msg0, msg1);

            // Rounds 16-19 (Ch, K0/K1 boundary)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1cq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg2, k1);
            msg3 = vsha1su1q_u32(msg3, msg2);
            msg0 = vsha1su0q_u32(msg0, msg1, msg2);

            // Kick off the next block's loads here — the
            // initial m0..m3 inputs are fully consumed by
            // the K0 quads above, and the remaining 60
            // rounds give the loads plenty of latency to
            // hide under. Branchless: the last iteration
            // redundantly re-reads its own block (a csel
            // instead of a mid-loop branch that would
            // split the round pipeline every block).
            let next_off = if i + 1 < n { (i + 1) * 64 } else { i * 64 };
            // SAFETY: `next_off / 64 < n` in both arms, so
            // the 64-byte read is fully in-bounds.
            (m0, m1, m2, m3) = unsafe { load_block(base.add(next_off)) };

            // Rounds 20-23 (Parity, K1)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg3, k1);
            msg0 = vsha1su1q_u32(msg0, msg3);
            msg1 = vsha1su0q_u32(msg1, msg2, msg3);

            // Rounds 24-27 (Parity, K1)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg0, k1);
            msg1 = vsha1su1q_u32(msg1, msg0);
            msg2 = vsha1su0q_u32(msg2, msg3, msg0);

            // Rounds 28-31 (Parity, K1)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg1, k1);
            msg2 = vsha1su1q_u32(msg2, msg1);
            msg3 = vsha1su0q_u32(msg3, msg0, msg1);

            // Rounds 32-35 (Parity, K1 → K2)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg2, k2);
            msg3 = vsha1su1q_u32(msg3, msg2);
            msg0 = vsha1su0q_u32(msg0, msg1, msg2);

            // Rounds 36-39 (Parity, K2 boundary)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg3, k2);
            msg0 = vsha1su1q_u32(msg0, msg3);
            msg1 = vsha1su0q_u32(msg1, msg2, msg3);

            // Rounds 40-43 (Maj, K2)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1mq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg0, k2);
            msg1 = vsha1su1q_u32(msg1, msg0);
            msg2 = vsha1su0q_u32(msg2, msg3, msg0);

            // Rounds 44-47 (Maj, K2)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1mq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg1, k2);
            msg2 = vsha1su1q_u32(msg2, msg1);
            msg3 = vsha1su0q_u32(msg3, msg0, msg1);

            // Rounds 48-51 (Maj, K2)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1mq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg2, k2);
            msg3 = vsha1su1q_u32(msg3, msg2);
            msg0 = vsha1su0q_u32(msg0, msg1, msg2);

            // Rounds 52-55 (Maj, K2 → K3)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1mq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg3, k3);
            msg0 = vsha1su1q_u32(msg0, msg3);
            msg1 = vsha1su0q_u32(msg1, msg2, msg3);

            // Rounds 56-59 (Maj, K3 boundary)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1mq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg0, k3);
            msg1 = vsha1su1q_u32(msg1, msg0);
            msg2 = vsha1su0q_u32(msg2, msg3, msg0);

            // Rounds 60-63 (Parity, K3)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg1, k3);
            msg2 = vsha1su1q_u32(msg2, msg1);
            msg3 = vsha1su0q_u32(msg3, msg0, msg1);

            // Rounds 64-67 (Parity, K3)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e0, tmp0);
            tmp0 = vaddq_u32(msg2, k3);
            msg3 = vsha1su1q_u32(msg3, msg2);

            // Rounds 68-71 (Parity, K3)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);
            tmp1 = vaddq_u32(msg3, k3);

            // Rounds 72-75 (Parity, K3)
            e1 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e0, tmp0);

            // Rounds 76-79 (Parity, K3)
            e0 = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = vsha1pq_u32(abcd, e1, tmp1);

            // Block-to-block E accumulation, kept in the FP
            // register file: a plain `e0.wrapping_add(...)`
            // compiles to a GPR add and forces two
            // fmov GPR↔FP round-trips per block (~10 cycles
            // on Apple cores — measured ~10% of the whole
            // block). Routing the add through a vector
            // lane lets LLVM keep the value in the SIMD
            // domain end-to-end.
            let ev = vaddq_u32(
                vsetq_lane_u32::<0>(e0, vdupq_n_u32(0)),
                vsetq_lane_u32::<0>(e0_saved, vdupq_n_u32(0)),
            );
            e0 = vgetq_lane_u32::<0>(ev);
            abcd = vaddq_u32(abcd, abcd_saved);
        }

        // SAFETY: same bounds argument as the load above.
        unsafe { vst1q_u32(state.as_mut_ptr(), abcd) };
        state[4] = e0;
    }
}

// ============================================================
// Scalar oracle (differential-test entry point).
// ============================================================

/// Full SHA-1 computed through the **scalar** block
/// processor only — bypasses the hardware dispatch in
/// [`compress_blocks`]. Exists so the test suite can
/// differential-test the hardware path against the
/// spec-readable reference on the same machine.
///
/// Not part of the public API surface (hidden from docs,
/// exempt from semver). Do not use outside tests.
#[doc(hidden)]
pub fn sha1_scalar_oracle(data: &[u8]) -> [u8; 20] {
    let mut state = H0;
    let mut chunks = data.chunks_exact(64);
    for block in &mut chunks {
        let mut buf = [0u8; 64];
        buf.copy_from_slice(block);
        process_block(&mut state, &buf);
    }
    let rem = chunks.remainder();

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut last = [0u8; 128];
    last[..rem.len()].copy_from_slice(rem);
    last[rem.len()] = 0x80;
    let total = if rem.len() < 56 {
        last[56..64].copy_from_slice(&bit_len.to_be_bytes());
        64
    } else {
        last[120..128].copy_from_slice(&bit_len.to_be_bytes());
        128
    };
    for block in last[..total].chunks_exact(64) {
        let mut buf = [0u8; 64];
        buf.copy_from_slice(block);
        process_block(&mut state, &buf);
    }

    let mut out = [0u8; 20];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ============================================================
// Block processor (RFC 3174 § 6.1 main loop).
// ============================================================

fn process_block(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, chunk) in block.chunks_exact(4).enumerate().take(16) {
        w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for t in 16..80 {
        w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];

    for (t, wt) in w.iter().enumerate() {
        let (f, k) = match t {
            0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*wt);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

#[cfg(test)]
mod compress_dispatch_tests {
    use super::*;

    #[test]
    fn empty_block_slice_is_a_noop() {
        // The hw path's `n == 0` early return is a safety
        // precondition for its raw 64-byte loads (an empty
        // input must not reach `load_block`). No public
        // caller currently passes an empty slice, but the
        // guard is part of the unsafe-boundary contract —
        // exercise it directly.
        let mut state = H0;
        compress_blocks(&mut state, &[]);
        assert_eq!(state, H0);
    }
}
