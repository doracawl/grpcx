#![doc = include_str!("../README.md")]
#![no_std]

//! RFC 4648 base64 encode + decode. Pure Rust, zero deps,
//! `no_std + alloc`. See the README for the spec citation
//! and feature matrix.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================
// Public ADTs.
// ============================================================

/// Which base64 alphabet to use.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Alphabet {
    /// RFC 4648 § 4 standard alphabet: `A-Z a-z 0-9 + /`.
    #[default]
    Standard,
    /// RFC 4648 § 5 URL-safe alphabet: `A-Z a-z 0-9 - _`.
    UrlSafe,
}

/// Decode strictness — controls how the decoder handles
/// missing `=` padding.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PaddingMode {
    /// RFC 4648 § 3.2 default: `=` padding is mandatory.
    /// Inputs missing required `=` are rejected.
    #[default]
    Strict,
    /// Padding-tolerant: missing `=` accepted as long as
    /// the input's length-mod-4 implies an unambiguous
    /// decode. Strict mode is preferred; use this only
    /// for interoperating with senders that drop padding
    /// (e.g. some JWT libraries).
    Tolerant,
}

/// Decode failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Input contains a character not in the chosen
    /// alphabet.
    InvalidCharacter,
    /// Input ended mid-quad (e.g. 1 character left after
    /// padding stripped — base64 can't carry 1-character
    /// remainder).
    InvalidLength,
    /// Missing required `=` padding (strict mode only).
    MissingPadding,
    /// Trailing bits in the last symbol are non-zero —
    /// indicates a malformed encoding per RFC 4648
    /// § 3.5.
    NonCanonical,
}

// ============================================================
// Encode.
// ============================================================

const ALPHABET_STANDARD: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const ALPHABET_URLSAFE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encode using the RFC 4648 § 4 standard alphabet with
/// mandatory `=` padding.
pub fn encode(data: &[u8]) -> String {
    encode_with(data, Alphabet::Standard)
}

/// Encode using the chosen alphabet.
pub fn encode_with(data: &[u8], alphabet: Alphabet) -> String {
    let table = match alphabet {
        Alphabet::Standard => ALPHABET_STANDARD,
        Alphabet::UrlSafe => ALPHABET_URLSAFE,
    };
    let out_len = data.len().div_ceil(3) * 4;
    // `with_capacity` + raw writes instead of a zero-fill:
    // zeroing the output buffer costs ~13% of total encode
    // time at 1 MiB and the writes below cover every byte.
    let mut out: Vec<u8> = Vec::with_capacity(out_len);

    let full_in = data.len() / 3 * 3;
    let rem_in = &data[full_in..];

    // SAFETY (whole block): exactly `out_len` bytes are
    // written through `dst` before `set_len(out_len)`:
    // 8 bytes per 6-byte step + 4 bytes per 3-byte step
    // (together covering `full_in / 3 * 4`) plus 4 bytes
    // for a 1- or 2-byte remainder — which is `out_len` by
    // the `div_ceil` above. All writes are in-bounds of
    // the freshly reserved capacity; all reads are bounded
    // by the explicit `i + 8 <= data.len()` /
    // `left >= 3` index arithmetic.
    unsafe {
        let mut dst = out.as_mut_ptr();
        let src = data.as_ptr();
        let mut i = 0usize;
        let mut left = full_in;

        // NEON lane: 48 input bytes → 64 ASCII bytes per
        // group via de-interleaved loads + a 64-byte
        // table lookup (vqtbl4q). Reads are exact (vld3q
        // consumes exactly 48 bytes — no overread).
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        {
            let groups = left / 48;
            if groups > 0 {
                // SAFETY: `groups * 48 <= left <= data.len()`
                // readable at `src`; `groups * 64` writable
                // at `dst` within the reserved `out_len`.
                neon::encode_groups(table, src, dst, groups);
                i += groups * 48;
                dst = dst.add(groups * 64);
                left -= groups * 48;
            }
        }

        // u64 lane: one unaligned 8-byte big-endian load
        // covers two 3-byte groups (48 payload bits in
        // bits 63..16) → 8 table lookups, zero per-byte
        // loads. Requires 8 readable bytes at `i`, which
        // may overlap the remainder bytes — they're still
        // in-bounds of `data`.
        while left >= 6 && i + 8 <= data.len() {
            let w = u64::from_be(core::ptr::read_unaligned(src.add(i).cast::<u64>()));
            *dst = table[(w >> 58) as usize & 0x3F];
            *dst.add(1) = table[(w >> 52) as usize & 0x3F];
            *dst.add(2) = table[(w >> 46) as usize & 0x3F];
            *dst.add(3) = table[(w >> 40) as usize & 0x3F];
            *dst.add(4) = table[(w >> 34) as usize & 0x3F];
            *dst.add(5) = table[(w >> 28) as usize & 0x3F];
            *dst.add(6) = table[(w >> 22) as usize & 0x3F];
            *dst.add(7) = table[(w >> 16) as usize & 0x3F];
            i += 6;
            dst = dst.add(8);
            left -= 6;
        }

        // 3-byte lane drains the last full triples the u64
        // lane couldn't take (input tail too short for an
        // 8-byte read).
        while left >= 3 {
            let w = (u32::from(*src.add(i)) << 16)
                | (u32::from(*src.add(i + 1)) << 8)
                | u32::from(*src.add(i + 2));
            *dst = table[(w >> 18) as usize & 0x3F];
            *dst.add(1) = table[(w >> 12) as usize & 0x3F];
            *dst.add(2) = table[(w >> 6) as usize & 0x3F];
            *dst.add(3) = table[w as usize & 0x3F];
            i += 3;
            dst = dst.add(4);
            left -= 3;
        }

        // `data.len() - full_in` is 0, 1, or 2 by
        // construction.
        if rem_in.len() == 1 {
            let b0 = rem_in[0];
            *dst = table[(b0 >> 2) as usize];
            *dst.add(1) = table[((b0 & 0x03) << 4) as usize];
            *dst.add(2) = b'=';
            *dst.add(3) = b'=';
        } else if rem_in.len() == 2 {
            let b0 = rem_in[0];
            let b1 = rem_in[1];
            *dst = table[(b0 >> 2) as usize];
            *dst.add(1) = table[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize];
            *dst.add(2) = table[((b1 & 0x0F) << 2) as usize];
            *dst.add(3) = b'=';
        }

        out.set_len(out_len);
    }

    // SAFETY: every byte in `out` came from the 64-entry
    // ASCII alphabet table or is b'=' — all single-byte
    // ASCII, therefore valid UTF-8.
    unsafe { String::from_utf8_unchecked(out) }
}

// ============================================================
// Decode.
// ============================================================

/// Decode using the RFC 4648 § 4 standard alphabet with
/// strict `=` padding requirement.
pub fn decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    decode_with(input, Alphabet::Standard)
}

/// Decode using the chosen alphabet, strict padding.
pub fn decode_with(input: &str, alphabet: Alphabet) -> Result<Vec<u8>, DecodeError> {
    decode_full(input.as_bytes(), alphabet, PaddingMode::Strict)
}

/// Decode using the chosen alphabet and padding mode.
pub fn decode_padded(
    input: &str,
    alphabet: Alphabet,
    padding: PaddingMode,
) -> Result<Vec<u8>, DecodeError> {
    decode_full(input.as_bytes(), alphabet, padding)
}

fn decode_full(
    input: &[u8],
    alphabet: Alphabet,
    padding: PaddingMode,
) -> Result<Vec<u8>, DecodeError> {
    let decode_table = match alphabet {
        Alphabet::Standard => &DECODE_STANDARD,
        Alphabet::UrlSafe => &DECODE_URLSAFE,
    };

    // Step 1: count and strip trailing '=' padding.
    let (effective_len, pad_count) = strip_padding(input)?;
    let body = &input[..effective_len];

    // Step 2: structural checks (length-mod-4 + padding
    // consistency). On failure, defer to the cold
    // diagnostic helper so that a body containing an
    // invalid character still reports InvalidCharacter
    // first — the error-priority contract (see
    // `invalid_character_rejected` in tests/kat.rs).
    let pad_implied = match effective_len % 4 {
        0 => 0,
        1 => {
            return Err(diagnose_structural(
                body,
                decode_table,
                DecodeError::InvalidLength,
            ));
        }
        2 => 2, // 2 base64 chars = 1 byte; padding implies "=="
        _ => 1, // 3 base64 chars = 2 bytes; padding implies "="
    };
    let pad_ok = match padding {
        PaddingMode::Strict => pad_count == pad_implied,
        PaddingMode::Tolerant => pad_count == 0 || pad_count == pad_implied,
    };
    if !pad_ok {
        return Err(diagnose_structural(
            body,
            decode_table,
            DecodeError::MissingPadding,
        ));
    }

    // Step 3: single-pass decode through four pre-shifted
    // u32 tables (D0..D3): each table maps an input byte
    // straight to its 6-bit value already shifted into
    // final position, so a quad becomes 4 loads + 3 ORs —
    // zero runtime shifts on the values. Invalid bytes map
    // to a sentinel with bit 31 set; OR-accumulating every
    // looked-up word into `acc` detects any bad input
    // without a per-quad branch. Output goes through raw
    // pointer writes into reserved (not zero-filled)
    // capacity — zeroing costs ~10% at 1 MiB.
    let [d0, d1, d2, d3] = match alphabet {
        Alphabet::Standard => &DECODE_STANDARD_W,
        Alphabet::UrlSafe => &DECODE_URLSAFE_W,
    };
    let full_quads = body.len() / 4;
    let tail_len = body.len() % 4; // 0, 2, or 3 (1 rejected above)
    let tail_bytes = match tail_len {
        0 => 0,
        2 => 1,
        _ => 2, // tail_len == 3
    };
    let out_len = full_quads * 3 + tail_bytes;

    // NEON fast path (standard alphabet): vectorized
    // whole-group decode. On any invalid byte it bails and
    // we fall through to the scalar path below, which
    // re-scans and reports the precise error per the
    // priority contract — invalid input is the cold case.
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    if matches!(alphabet, Alphabet::Standard) {
        let groups = full_quads / 16; // 16 quads = 64 chars per group
        if groups > 0 {
            let mut out: Vec<u8> = Vec::with_capacity(out_len + 2);
            // SAFETY: `groups * 64 <= body.len()` readable;
            // `groups * 48 <= out_len` writable within the
            // reserved capacity.
            let ok =
                unsafe { neon::decode_groups_standard(body.as_ptr(), out.as_mut_ptr(), groups) };
            if ok {
                // Scalar continuation for the remaining
                // quads + tail, writing past the NEON
                // output. Reuses the same single-pass
                // scalar logic via `decode_scalar_into`.
                // SAFETY: dst offset `groups * 48` plus the
                // scalar continuation's writes stay within
                // `out_len + 2`; `set_len` happens inside
                // on success.
                match unsafe {
                    decode_scalar_into(
                        &body[groups * 64..],
                        decode_table,
                        (d0, d1, d2, d3),
                        tail_len,
                        out,
                        groups * 48,
                        out_len,
                    )
                } {
                    Ok(v) => return Ok(v),
                    Err(e) => return Err(e),
                }
            }
            // ok == false → invalid byte somewhere in the
            // vector body; fall through to the scalar path
            // for exact error reporting.
        }
    }
    // Scalar path. +2 capacity slack: the two-quad lane
    // inside `decode_scalar_into` emits 6 payload bytes
    // per 8-byte store.
    let out: Vec<u8> = Vec::with_capacity(out_len + 2);
    // SAFETY: fresh buffer, offset 0 — `decode_scalar_into`
    // writes exactly `out_len` bytes into the reserved
    // `out_len + 2` capacity.
    unsafe {
        decode_scalar_into(
            body,
            decode_table,
            (d0, d1, d2, d3),
            tail_len,
            out,
            0,
            out_len,
        )
    }
}

/// Single-pass scalar decode of `rest` (full quads + the
/// 0/2/3-char tail) appended at byte offset `dst_offset`
/// of `out`'s reserved capacity. Shared by the pure-scalar
/// path (`dst_offset == 0`) and the NEON path's
/// continuation (`dst_offset == groups * 48`).
///
/// # Safety
/// `out` must have at least `out_len + 2` reserved
/// capacity, and `dst_offset` plus this function's writes
/// (`rest.len() / 4 * 3` + tail bytes, +2 slack for the
/// two-quad lane) must not exceed it. On success the
/// buffer's length is set to `out_len`.
unsafe fn decode_scalar_into(
    rest: &[u8],
    decode_table: &[u8; 256],
    (d0, d1, d2, d3): (&[u32; 256], &[u32; 256], &[u32; 256], &[u32; 256]),
    tail_len: usize,
    mut out: Vec<u8>,
    dst_offset: usize,
    out_len: usize,
) -> Result<Vec<u8>, DecodeError> {
    let rest_quads = rest.len() / 4;
    let (in_quads, in_tail) = rest.split_at(rest_quads * 4);
    let mut acc: u32 = 0;

    // SAFETY (whole block): writes start at `dst_offset`
    // and cover exactly `rest_quads * 3` + tail bytes
    // (+2 slack per the two-quad stores), which the caller
    // guarantees fits in the reserved capacity.
    unsafe {
        let mut dst = out.as_mut_ptr().add(dst_offset);

        // Two-quad lane: 8 input chars → 8 table loads →
        // one big-endian u64 store carrying both 24-bit
        // groups (48 payload bits in bits 63..16).
        let mut pairs = in_quads.chunks_exact(8);
        for q in &mut pairs {
            let w0 = d0[q[0] as usize] | d1[q[1] as usize] | d2[q[2] as usize] | d3[q[3] as usize];
            let w1 = d0[q[4] as usize] | d1[q[5] as usize] | d2[q[6] as usize] | d3[q[7] as usize];
            acc |= w0 | w1;
            let combined = (u64::from(w0) << 40) | (u64::from(w1) << 16);
            core::ptr::write_unaligned(dst.cast::<u64>(), combined.to_be());
            dst = dst.add(6);
        }

        // Single-quad drain (0 or 1 iteration).
        for q in pairs.remainder().chunks_exact(4) {
            let w = d0[q[0] as usize] | d1[q[1] as usize] | d2[q[2] as usize] | d3[q[3] as usize];
            acc |= w;
            *dst = (w >> 16) as u8;
            *dst.add(1) = (w >> 8) as u8;
            *dst.add(2) = w as u8;
            dst = dst.add(3);
        }

        if tail_len == 2 {
            let c0 = decode_table[in_tail[0] as usize];
            let c1 = decode_table[in_tail[1] as usize];
            acc |= (u32::from(c0) | u32::from(c1)) << 24;
            // Invalid characters outrank NonCanonical in
            // the error-priority contract — check acc
            // first (bit 31 set by either the u32 tables
            // or the byte-table INVALID shifted here).
            if acc >> 31 != 0 {
                return Err(DecodeError::InvalidCharacter);
            }
            // The 2 base64 chars carry 12 bits, but only 8
            // become the output byte; the low 4 bits of c1
            // MUST be zero (RFC 4648 § 3.5).
            if c1 & 0x0F != 0 {
                return Err(DecodeError::NonCanonical);
            }
            *dst = (c0 << 2) | (c1 >> 4);
        } else if tail_len == 3 {
            let c0 = decode_table[in_tail[0] as usize];
            let c1 = decode_table[in_tail[1] as usize];
            let c2 = decode_table[in_tail[2] as usize];
            acc |= (u32::from(c0) | u32::from(c1) | u32::from(c2)) << 24;
            if acc >> 31 != 0 {
                return Err(DecodeError::InvalidCharacter);
            }
            // The 3 base64 chars carry 18 bits, but only
            // 16 become output bytes; the low 2 bits of c2
            // MUST be zero.
            if c2 & 0x03 != 0 {
                return Err(DecodeError::NonCanonical);
            }
            *dst = (c0 << 2) | (c1 >> 4);
            *dst.add(1) = (c1 << 4) | (c2 >> 2);
        }

        if acc >> 31 != 0 {
            return Err(DecodeError::InvalidCharacter);
        }

        out.set_len(out_len);
    }

    Ok(out)
}

/// Cold path: a structural check failed (bad length /
/// missing padding). Scan the body for an invalid
/// character first — InvalidCharacter outranks structural
/// errors in the reporting contract — and fall back to the
/// structural error otherwise.
#[cold]
fn diagnose_structural(body: &[u8], table: &[u8; 256], structural: DecodeError) -> DecodeError {
    for &b in body {
        if table[b as usize] == INVALID {
            return DecodeError::InvalidCharacter;
        }
    }
    structural
}

fn strip_padding(input: &[u8]) -> Result<(usize, usize), DecodeError> {
    // Padding can only appear at the very end. RFC 4648
    // § 3.2 specifies at most 2 `=` characters.
    let mut len = input.len();
    let mut pad = 0;
    while len > 0 && input[len - 1] == b'=' {
        if pad == 2 {
            return Err(DecodeError::InvalidLength);
        }
        pad += 1;
        len -= 1;
    }
    Ok((len, pad))
}

const INVALID: u8 = 0xFF;

/// Reverse lookup table for the standard alphabet.
/// `INVALID` (`0xFF`) marks any byte not in the alphabet.
const DECODE_STANDARD: [u8; 256] =
    build_decode_table(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/");

/// Reverse lookup table for the URL-safe alphabet.
const DECODE_URLSAFE: [u8; 256] =
    build_decode_table(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_");

const fn build_decode_table(alphabet: &[u8; 64]) -> [u8; 256] {
    let mut table = [INVALID; 256];
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

/// Sentinel bit for the pre-shifted u32 decode tables —
/// valid entries only occupy bits 0..24, so bit 31 marks
/// "input byte not in the alphabet".
const INVALID_W: u32 = 1 << 31;

/// Pre-shifted u32 decode tables for the standard
/// alphabet: `D[k][b]` is the 6-bit value of input byte
/// `b` already shifted into position `18 - 6k` of the
/// 24-bit quad word, or [`INVALID_W`]. A quad decodes as
/// `D0[q0] | D1[q1] | D2[q2] | D3[q3]` — no runtime
/// shifts on the values.
const DECODE_STANDARD_W: [[u32; 256]; 4] = build_shifted_tables(&DECODE_STANDARD);

/// Pre-shifted u32 decode tables for the URL-safe
/// alphabet.
const DECODE_URLSAFE_W: [[u32; 256]; 4] = build_shifted_tables(&DECODE_URLSAFE);

const fn build_shifted_tables(byte_table: &[u8; 256]) -> [[u32; 256]; 4] {
    let mut out = [[INVALID_W; 256]; 4];
    let mut b = 0;
    while b < 256 {
        let v = byte_table[b];
        if v != INVALID {
            out[0][b] = (v as u32) << 18;
            out[1][b] = (v as u32) << 12;
            out[2][b] = (v as u32) << 6;
            out[3][b] = v as u32;
        }
        b += 1;
    }
    out
}

// ============================================================
// NEON lanes (AArch64).
//
// Encode: the aklomp/Muła reshuffle — de-interleave 48
// input bytes into 3 byte-planes (vld3q), recombine into 4
// six-bit planes with shifts/ORs, map each plane through
// the 64-byte alphabet held in 4 vector registers
// (vqtbl4q), and interleave-store 64 ASCII bytes (vst4q).
//
// Decode (standard alphabet only): the Muła nibble-LUT
// scheme — classify each byte by (high nibble, low nibble)
// through two 16-byte masks whose AND is non-zero exactly
// for bytes outside the alphabet, then translate via a
// per-high-nibble delta (with the '/' special case folded
// in through an equality mask). 64 input chars become 48
// output bytes per group (vld4q → pack → vst3q). Any
// invalid byte anywhere in the vector body makes the
// caller fall back to the scalar path, which re-scans and
// reports the precise error per the priority contract.
//
// `unsafe` is confined to the raw vector loads/stores;
// arithmetic intrinsics are value-based. Correctness
// gates: the full KAT suite (round-trips at every length
// 0..=300), plus `neon_invalid_classification_matches_
// scalar_table` differential-testing the validity masks
// against the scalar decode table for every byte value.
// ============================================================

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use core::arch::aarch64::*;

    /// Encode `groups` × 48 bytes from `src` into
    /// `groups` × 64 ASCII bytes at `dst`.
    ///
    /// # Safety
    /// `src` must have `groups * 48` readable bytes; `dst`
    /// must have `groups * 64` writable bytes.
    #[target_feature(enable = "neon")]
    pub(super) unsafe fn encode_groups(
        table: &[u8; 64],
        src: *const u8,
        dst: *mut u8,
        groups: usize,
    ) {
        // SAFETY: `table` is exactly 64 bytes — 4 × 16.
        let tbl = unsafe { vld1q_u8_x4(table.as_ptr()) };
        let mask_03 = vdupq_n_u8(0x03);
        let mask_0f = vdupq_n_u8(0x0F);
        let mask_3f = vdupq_n_u8(0x3F);

        for g in 0..groups {
            // SAFETY: caller guarantees group `g` has 48
            // readable / 64 writable bytes.
            let input = unsafe { vld3q_u8(src.add(g * 48)) };

            // Byte planes → 6-bit planes (RFC 4648 § 4
            // bit slicing, 16 triples at a time).
            let v0 = vshrq_n_u8::<2>(input.0);
            let v1 = vorrq_u8(
                vshlq_n_u8::<4>(vandq_u8(input.0, mask_03)),
                vshrq_n_u8::<4>(input.1),
            );
            let v2 = vorrq_u8(
                vshlq_n_u8::<2>(vandq_u8(input.1, mask_0f)),
                vshrq_n_u8::<6>(input.2),
            );
            let v3 = vandq_u8(input.2, mask_3f);

            let out = uint8x16x4_t(
                vqtbl4q_u8(tbl, v0),
                vqtbl4q_u8(tbl, v1),
                vqtbl4q_u8(tbl, v2),
                vqtbl4q_u8(tbl, v3),
            );
            // SAFETY: see above.
            unsafe { vst4q_u8(dst.add(g * 64), out) };
        }
    }

    // Validity masks (standard alphabet): for input byte
    // `b`, `LUT_LO[b & 0xF] & LUT_HI[b >> 4]` is non-zero
    // iff `b` is NOT in the alphabet. The widely-used
    // constants from Muła's base64simd (also in simdjson);
    // differentially verified against the scalar decode
    // table for all 256 byte values in the test suite.
    const LUT_LO: [u8; 16] = [
        0x15, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x13, 0x1A, 0x1B, 0x1B, 0x1B,
        0x1A,
    ];
    const LUT_HI: [u8; 16] = [
        0x10, 0x10, 0x01, 0x02, 0x04, 0x08, 0x04, 0x08, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
        0x10,
    ];
    // Per-high-nibble ASCII→value delta. '/' (0x2F) shares
    // high nibble 2 with '+' (0x2B); an equality mask adds
    // 0xFF (-1) to its index, routing it to slot 1.
    const LUT_ROLL: [u8; 16] = [
        0x00, 16, 19, 4, 191, 191, 185, 185, // 191 = -65, 185 = -71 (mod 256)
        0, 0, 0, 0, 0, 0, 0, 0,
    ];

    /// Pre-loaded decode LUT registers, hoisted out of the
    /// group loop (3 vector loads per call would otherwise
    /// repeat 12× per group).
    struct DecodeLuts {
        lo: uint8x16_t,
        hi: uint8x16_t,
        roll: uint8x16_t,
    }

    /// Map one 16-byte plane of base64 characters to their
    /// 6-bit values; OR classification bits into
    /// `invalid_acc` (non-zero lane = bad byte).
    #[inline]
    #[target_feature(enable = "neon")]
    fn map_plane(v: uint8x16_t, luts: &DecodeLuts, invalid_acc: &mut uint8x16_t) -> uint8x16_t {
        let hi = vshrq_n_u8::<4>(v);
        let lo = vandq_u8(v, vdupq_n_u8(0x0F));
        let class = vandq_u8(vqtbl1q_u8(luts.lo, lo), vqtbl1q_u8(luts.hi, hi));
        *invalid_acc = vorrq_u8(*invalid_acc, class);
        let eq_2f = vceqq_u8(v, vdupq_n_u8(0x2F));
        let roll_idx = vaddq_u8(hi, eq_2f); // eq mask = 0xFF = -1
        vaddq_u8(v, vqtbl1q_u8(luts.roll, roll_idx))
    }

    /// Decode `groups` × 64 standard-alphabet chars from
    /// `src` into `groups` × 48 bytes at `dst`. Returns
    /// `false` if any byte was outside the alphabet (the
    /// output is then garbage and the caller must fall
    /// back to the scalar path).
    ///
    /// # Safety
    /// `src` must have `groups * 64` readable bytes; `dst`
    /// must have `groups * 48` writable bytes.
    #[target_feature(enable = "neon")]
    pub(super) unsafe fn decode_groups_standard(
        src: *const u8,
        dst: *mut u8,
        groups: usize,
    ) -> bool {
        let mut invalid = vdupq_n_u8(0);
        // SAFETY: the three LUT arrays are exactly 16 bytes.
        let luts = unsafe {
            DecodeLuts {
                lo: vld1q_u8(LUT_LO.as_ptr()),
                hi: vld1q_u8(LUT_HI.as_ptr()),
                roll: vld1q_u8(LUT_ROLL.as_ptr()),
            }
        };

        for g in 0..groups {
            // vld4q de-interleaves 64 chars so that the 4
            // chars of each quad land in the same lane of
            // the 4 planes — the pack below is then pure
            // lane-wise shifts/ORs.
            // SAFETY: caller guarantees group `g` has 64
            // readable / 48 writable bytes.
            let q = unsafe { vld4q_u8(src.add(g * 64)) };
            let c0 = map_plane(q.0, &luts, &mut invalid);
            let c1 = map_plane(q.1, &luts, &mut invalid);
            let c2 = map_plane(q.2, &luts, &mut invalid);
            let c3 = map_plane(q.3, &luts, &mut invalid);

            // u8 shifts self-truncate, so no pre-masking
            // is needed: `c1 << 4` keeps exactly the low
            // nibble's contribution, etc.
            let b0 = vorrq_u8(vshlq_n_u8::<2>(c0), vshrq_n_u8::<4>(c1));
            let b1 = vorrq_u8(vshlq_n_u8::<4>(c1), vshrq_n_u8::<2>(c2));
            let b2 = vorrq_u8(vshlq_n_u8::<6>(c2), c3);

            // SAFETY: see above.
            unsafe { vst3q_u8(dst.add(g * 48), uint8x16x3_t(b0, b1, b2)) };
        }

        vmaxvq_u8(invalid) == 0
    }
}

#[cfg(test)]
mod build_decode_table_runtime_check {
    //! `build_decode_table` is invoked from runtime test
    //! context so coverage instrumentation registers the
    //! function (const-eval pre-computation otherwise
    //! shadows it from llvm-cov). Doubles as a sanity
    //! check that the runtime + const-eval paths agree.
    use super::*;

    #[test]
    fn standard_table_matches_const() {
        let runtime = build_decode_table(ALPHABET_STANDARD);
        assert_eq!(runtime, DECODE_STANDARD);
    }

    #[test]
    fn urlsafe_table_matches_const() {
        let runtime = build_decode_table(ALPHABET_URLSAFE);
        assert_eq!(runtime, DECODE_URLSAFE);
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    #[test]
    fn neon_invalid_classification_matches_scalar_table() {
        // For every byte value, plant it inside a 64-char
        // NEON group (68-char input = 1 NEON group + 1
        // scalar quad) and check that the vector path's
        // accept/reject + decoded bytes agree with the
        // scalar decode table exactly.
        for b in 0..=255u8 {
            let mut input = [b'A'; 68];
            input[0] = b;
            let got = decode_full(&input, Alphabet::Standard, PaddingMode::Strict);
            if DECODE_STANDARD[b as usize] == INVALID {
                assert_eq!(
                    got.unwrap_err(),
                    DecodeError::InvalidCharacter,
                    "byte {b:#04x} must be rejected"
                );
            } else {
                // Scalar reference: 17 quads, no tail.
                let mut expect = Vec::new();
                for q in input.chunks_exact(4) {
                    let w = (u32::from(DECODE_STANDARD[q[0] as usize]) << 18)
                        | (u32::from(DECODE_STANDARD[q[1] as usize]) << 12)
                        | (u32::from(DECODE_STANDARD[q[2] as usize]) << 6)
                        | u32::from(DECODE_STANDARD[q[3] as usize]);
                    expect.push((w >> 16) as u8);
                    expect.push((w >> 8) as u8);
                    expect.push(w as u8);
                }
                assert_eq!(got.unwrap(), expect, "byte {b:#04x} decoded value");
            }
        }
    }

    #[test]
    fn shifted_tables_match_const() {
        let std_w = build_shifted_tables(&DECODE_STANDARD);
        assert_eq!(std_w, DECODE_STANDARD_W);
        let url_w = build_shifted_tables(&DECODE_URLSAFE);
        assert_eq!(url_w, DECODE_URLSAFE_W);
        // Spot-check the shift positions + sentinel.
        assert_eq!(std_w[0][b'A' as usize], 0);
        assert_eq!(std_w[3][b'B' as usize], 1);
        assert_eq!(std_w[2][b'B' as usize], 1 << 6);
        assert_eq!(std_w[1][b'B' as usize], 1 << 12);
        assert_eq!(std_w[0][b'B' as usize], 1 << 18);
        assert_eq!(std_w[0][b' ' as usize], INVALID_W);
    }
}
