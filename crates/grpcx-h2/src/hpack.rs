//! HPACK — RFC 7541 header compression for HTTP/2.
//!
//! Adapted from L5's QPACK (RFC 9204 is QPACK's QUIC
//! descendant; the prefixed-integer codec is identical,
//! the static table differs, and HPACK's dynamic table is
//! simpler — synchronous indexing, no encoder/decoder
//! streams).
//!
//! Decode side is complete (incl. Huffman — peers like
//! hyper/h2 emit Huffman-coded literals by default).
//! Encode side emits indexed names + literal values
//! without Huffman: always legal, simple, and gRPC header
//! values (paths, status digits, binary metadata) gain
//! little from Huffman anyway.
//!
//! Correctness gates: RFC 7541 Appendix C test vectors
//! (C.2 literals, C.3 request sequences, C.4 Huffman
//! request sequences, C.5/C.6 response sequences) +
//! a Huffman-table prefix-freedom self-check + the live
//! tonic interop e2e (T6).

use alloc::string::String;
use alloc::vec::Vec;

/// HPACK failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpackError {
    /// Input ended mid-field.
    Truncated,
    /// Index outside static + dynamic table bounds.
    BadIndex,
    /// Prefixed-integer overflow / malformed varint chain.
    BadInt,
    /// Invalid Huffman padding or EOS in the stream.
    BadHuffman,
    /// Dynamic-table size update above the negotiated cap.
    BadTableSize,
}

// ============================================================
// Static table (RFC 7541 Appendix A — 61 entries, 1-based).
// ============================================================

const STATIC_TABLE: &[(&[u8], &[u8])] = &[
    (b":authority", b""),
    (b":method", b"GET"),
    (b":method", b"POST"),
    (b":path", b"/"),
    (b":path", b"/index.html"),
    (b":scheme", b"http"),
    (b":scheme", b"https"),
    (b":status", b"200"),
    (b":status", b"204"),
    (b":status", b"206"),
    (b":status", b"304"),
    (b":status", b"400"),
    (b":status", b"404"),
    (b":status", b"500"),
    (b"accept-charset", b""),
    (b"accept-encoding", b"gzip, deflate"),
    (b"accept-language", b""),
    (b"accept-ranges", b""),
    (b"accept", b""),
    (b"access-control-allow-origin", b""),
    (b"age", b""),
    (b"allow", b""),
    (b"authorization", b""),
    (b"cache-control", b""),
    (b"content-disposition", b""),
    (b"content-encoding", b""),
    (b"content-language", b""),
    (b"content-length", b""),
    (b"content-location", b""),
    (b"content-range", b""),
    (b"content-type", b""),
    (b"cookie", b""),
    (b"date", b""),
    (b"etag", b""),
    (b"expect", b""),
    (b"expires", b""),
    (b"from", b""),
    (b"host", b""),
    (b"if-match", b""),
    (b"if-modified-since", b""),
    (b"if-none-match", b""),
    (b"if-range", b""),
    (b"if-unmodified-since", b""),
    (b"last-modified", b""),
    (b"link", b""),
    (b"location", b""),
    (b"max-forwards", b""),
    (b"proxy-authenticate", b""),
    (b"proxy-authorization", b""),
    (b"range", b""),
    (b"referer", b""),
    (b"refresh", b""),
    (b"retry-after", b""),
    (b"server", b""),
    (b"set-cookie", b""),
    (b"strict-transport-security", b""),
    (b"transfer-encoding", b""),
    (b"user-agent", b""),
    (b"vary", b""),
    (b"via", b""),
    (b"www-authenticate", b""),
];

// ============================================================
// Prefixed integers (RFC 7541 §5.1) — same codec as QPACK
// (lifted from grpcx-h3/src/qpack.rs).
// ============================================================

fn read_prefixed_int(
    buf: &mut &[u8],
    prefix_bits: u8,
    first_byte_value: u8,
) -> Result<u64, HpackError> {
    debug_assert!((1..=8).contains(&prefix_bits));
    let max_prefix: u8 = ((1u16 << prefix_bits) - 1) as u8;
    if first_byte_value < max_prefix {
        return Ok(first_byte_value as u64);
    }
    let mut value: u64 = max_prefix as u64;
    let mut shift: u32 = 0;
    loop {
        let &[byte, ref rest @ ..] = *buf else {
            return Err(HpackError::Truncated);
        };
        *buf = rest;
        let chunk = (byte & 0x7f) as u64;
        if shift >= 64 {
            return Err(HpackError::BadInt);
        }
        value = value
            .checked_add(chunk.checked_shl(shift).ok_or(HpackError::BadInt)?)
            .ok_or(HpackError::BadInt)?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

fn write_prefixed_int(value: u64, prefix_bits: u8, flags: u8, out: &mut Vec<u8>) {
    debug_assert!((1..=8).contains(&prefix_bits));
    let max_prefix = (1u64 << prefix_bits) - 1;
    if value < max_prefix {
        out.push(flags | (value as u8));
        return;
    }
    out.push(flags | (max_prefix as u8));
    let mut rest = value - max_prefix;
    while rest >= 0x80 {
        out.push(((rest & 0x7f) as u8) | 0x80);
        rest >>= 7;
    }
    out.push(rest as u8);
}

// ============================================================
// Huffman code (RFC 7541 Appendix B). Decode only.
//
// 256 symbol entries as (code, bits); EOS (30×1 bits) must
// never appear complete in a valid stream. The
// `huffman_table_is_prefix_free` test self-checks the
// transcription; Appendix C.4/C.6 vectors check it against
// the spec; the tonic interop e2e checks it against the
// world.
// ============================================================

#[rustfmt::skip]
const HUFFMAN: [(u32, u8); 256] = [
    (0x1ff8, 13), (0x7fffd8, 23), (0xfffffe2, 28), (0xfffffe3, 28),
    (0xfffffe4, 28), (0xfffffe5, 28), (0xfffffe6, 28), (0xfffffe7, 28),
    (0xfffffe8, 28), (0xffffea, 24), (0x3ffffffc, 30), (0xfffffe9, 28),
    (0xfffffea, 28), (0x3ffffffd, 30), (0xfffffeb, 28), (0xfffffec, 28),
    (0xfffffed, 28), (0xfffffee, 28), (0xfffffef, 28), (0xffffff0, 28),
    (0xffffff1, 28), (0xffffff2, 28), (0x3ffffffe, 30), (0xffffff3, 28),
    (0xffffff4, 28), (0xffffff5, 28), (0xffffff6, 28), (0xffffff7, 28),
    (0xffffff8, 28), (0xffffff9, 28), (0xffffffa, 28), (0xffffffb, 28),
    (0x14, 6), (0x3f8, 10), (0x3f9, 10), (0xffa, 12),
    (0x1ff9, 13), (0x15, 6), (0xf8, 8), (0x7fa, 11),
    (0x3fa, 10), (0x3fb, 10), (0xf9, 8), (0x7fb, 11),
    (0xfa, 8), (0x16, 6), (0x17, 6), (0x18, 6),
    (0x0, 5), (0x1, 5), (0x2, 5), (0x19, 6),
    (0x1a, 6), (0x1b, 6), (0x1c, 6), (0x1d, 6),
    (0x1e, 6), (0x1f, 6), (0x5c, 7), (0xfb, 8),
    (0x7ffc, 15), (0x20, 6), (0xffb, 12), (0x3fc, 10),
    (0x1ffa, 13), (0x21, 6), (0x5d, 7), (0x5e, 7),
    (0x5f, 7), (0x60, 7), (0x61, 7), (0x62, 7),
    (0x63, 7), (0x64, 7), (0x65, 7), (0x66, 7),
    (0x67, 7), (0x68, 7), (0x69, 7), (0x6a, 7),
    (0x6b, 7), (0x6c, 7), (0x6d, 7), (0x6e, 7),
    (0x6f, 7), (0x70, 7), (0x71, 7), (0x72, 7),
    (0xfc, 8), (0x73, 7), (0xfd, 8), (0x1ffb, 13),
    (0x7fff0, 19), (0x1ffc, 13), (0x3ffc, 14), (0x22, 6),
    (0x7ffd, 15), (0x3, 5), (0x23, 6), (0x4, 5),
    (0x24, 6), (0x5, 5), (0x25, 6), (0x26, 6),
    (0x27, 6), (0x6, 5), (0x74, 7), (0x75, 7),
    (0x28, 6), (0x29, 6), (0x2a, 6), (0x7, 5),
    (0x2b, 6), (0x76, 7), (0x2c, 6), (0x8, 5),
    (0x9, 5), (0x2d, 6), (0x77, 7), (0x78, 7),
    (0x79, 7), (0x7a, 7), (0x7b, 7), (0x7ffe, 15),
    (0x7fc, 11), (0x3ffd, 14), (0x1ffd, 13), (0xffffffc, 28),
    (0xfffe6, 20), (0x3fffd2, 22), (0xfffe7, 20), (0xfffe8, 20),
    (0x3fffd3, 22), (0x3fffd4, 22), (0x3fffd5, 22), (0x7fffd9, 23),
    (0x3fffd6, 22), (0x7fffda, 23), (0x7fffdb, 23), (0x7fffdc, 23),
    (0x7fffdd, 23), (0x7fffde, 23), (0xffffeb, 24), (0x7fffdf, 23),
    (0xffffec, 24), (0xffffed, 24), (0x3fffd7, 22), (0x7fffe0, 23),
    (0xffffee, 24), (0x7fffe1, 23), (0x7fffe2, 23), (0x7fffe3, 23),
    (0x7fffe4, 23), (0x1fffdc, 21), (0x3fffd8, 22), (0x7fffe5, 23),
    (0x3fffd9, 22), (0x7fffe6, 23), (0x7fffe7, 23), (0xffffef, 24),
    (0x3fffda, 22), (0x1fffdd, 21), (0xfffe9, 20), (0x3fffdb, 22),
    (0x3fffdc, 22), (0x7fffe8, 23), (0x7fffe9, 23), (0x1fffde, 21),
    (0x7fffea, 23), (0x3fffdd, 22), (0x3fffde, 22), (0xfffff0, 24),
    (0x1fffdf, 21), (0x3fffdf, 22), (0x7fffeb, 23), (0x7fffec, 23),
    (0x1fffe0, 21), (0x1fffe1, 21), (0x3fffe0, 22), (0x1fffe2, 21),
    (0x7fffed, 23), (0x3fffe1, 22), (0x7fffee, 23), (0x7fffef, 23),
    (0xfffea, 20), (0x3fffe2, 22), (0x3fffe3, 22), (0x3fffe4, 22),
    (0x7ffff0, 23), (0x3fffe5, 22), (0x3fffe6, 22), (0x7ffff1, 23),
    (0x3ffffe0, 26), (0x3ffffe1, 26), (0xfffeb, 20), (0x7fff1, 19),
    (0x3fffe7, 22), (0x7ffff2, 23), (0x3fffe8, 22), (0x1ffffec, 25),
    (0x3ffffe2, 26), (0x3ffffe3, 26), (0x3ffffe4, 26), (0x7ffffde, 27),
    (0x7ffffdf, 27), (0x3ffffe5, 26), (0xfffff1, 24), (0x1ffffed, 25),
    (0x7fff2, 19), (0x1fffe3, 21), (0x3ffffe6, 26), (0x7ffffe0, 27),
    (0x7ffffe1, 27), (0x3ffffe7, 26), (0x7ffffe2, 27), (0xfffff2, 24),
    (0x1fffe4, 21), (0x1fffe5, 21), (0x3ffffe8, 26), (0x3ffffe9, 26),
    (0xffffffd, 28), (0x7ffffe3, 27), (0x7ffffe4, 27), (0x7ffffe5, 27),
    (0xfffec, 20), (0xfffff3, 24), (0xfffed, 20), (0x1fffe6, 21),
    (0x3fffe9, 22), (0x1fffe7, 21), (0x1fffe8, 21), (0x7ffff3, 23),
    (0x3fffea, 22), (0x3fffeb, 22), (0x1ffffee, 25), (0x1ffffef, 25),
    (0xfffff4, 24), (0xfffff5, 24), (0x3ffffea, 26), (0x7ffff4, 23),
    (0x3ffffeb, 26), (0x7ffffe6, 27), (0x3ffffec, 26), (0x3ffffed, 26),
    (0x7ffffe7, 27), (0x7ffffe8, 27), (0x7ffffe9, 27), (0x7ffffea, 27),
    (0x7ffffeb, 27), (0xffffffe, 28), (0x7ffffec, 27), (0x7ffffed, 27),
    (0x7ffffee, 27), (0x7ffffef, 27), (0x7fffff0, 27), (0x3ffffee, 26),
];

/// Decode a Huffman-coded string (RFC 7541 §5.2).
/// Bit-serial table walk — headers are short; correctness
/// first, the FSM fast path is a v1.x option.
pub fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, HpackError> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut acc: u32 = 0;
    let mut acc_len: u8 = 0;
    for &byte in data {
        for bit in (0..8).rev() {
            acc = (acc << 1) | ((byte >> bit) & 1) as u32;
            acc_len += 1;
            if acc_len > 30 {
                return Err(HpackError::BadHuffman);
            }
            // Linear probe — the table is prefix-free, so
            // an (acc, acc_len) match is unambiguous.
            if let Some(sym) = HUFFMAN
                .iter()
                .position(|&(code, bits)| bits == acc_len && code == acc)
            {
                out.push(sym as u8);
                acc = 0;
                acc_len = 0;
            }
        }
    }
    // Remaining bits must be the EOS prefix: all ones,
    // strictly fewer than 8 bits (RFC 7541 §5.2).
    if acc_len >= 8 || (acc_len > 0 && acc != (1u32 << acc_len) - 1) {
        return Err(HpackError::BadHuffman);
    }
    Ok(out)
}

// ============================================================
// Dynamic table (RFC 7541 §4) — synchronous, evict-by-size.
// ============================================================

/// HPACK dynamic table. Entry size = name + value + 32
/// (RFC 7541 §4.1). Index space: 1..=61 static, 62+
/// dynamic (most-recent first).
pub struct DynamicTable {
    entries: alloc::collections::VecDeque<(Vec<u8>, Vec<u8>)>,
    size: usize,
    max_size: usize,
    /// Protocol cap (SETTINGS_HEADER_TABLE_SIZE); dynamic
    /// size updates may lower below but never exceed it.
    cap: usize,
}

impl DynamicTable {
    /// Construct with the protocol cap (both `max_size`
    /// and the bound for future size updates).
    pub fn new(cap: usize) -> Self {
        DynamicTable {
            entries: alloc::collections::VecDeque::new(),
            size: 0,
            max_size: cap,
            cap,
        }
    }

    fn entry_size(name: &[u8], value: &[u8]) -> usize {
        name.len() + value.len() + 32
    }

    fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) {
        let sz = Self::entry_size(&name, &value);
        // An oversized entry empties the table (§4.4).
        while self.size + sz > self.max_size {
            match self.entries.pop_back() {
                Some((n, v)) => self.size -= Self::entry_size(&n, &v),
                None => break,
            }
        }
        if sz <= self.max_size {
            self.size += sz;
            self.entries.push_front((name, value));
        }
    }

    fn set_max_size(&mut self, new: usize) -> Result<(), HpackError> {
        if new > self.cap {
            return Err(HpackError::BadTableSize);
        }
        self.max_size = new;
        while self.size > self.max_size {
            let (n, v) = self.entries.pop_back().expect("size>0 implies entries");
            self.size -= Self::entry_size(&n, &v);
        }
        Ok(())
    }

    fn get(&self, index: u64) -> Result<(&[u8], &[u8]), HpackError> {
        if index == 0 {
            return Err(HpackError::BadIndex);
        }
        let i = index as usize;
        if i <= STATIC_TABLE.len() {
            let (n, v) = STATIC_TABLE[i - 1];
            Ok((n, v))
        } else {
            let di = i - STATIC_TABLE.len() - 1;
            self.entries
                .get(di)
                .map(|(n, v)| (n.as_slice(), v.as_slice()))
                .ok_or(HpackError::BadIndex)
        }
    }
}

// ============================================================
// Header-block decode (RFC 7541 §6).
// ============================================================

fn read_string(buf: &mut &[u8]) -> Result<Vec<u8>, HpackError> {
    let &[first, ref rest @ ..] = *buf else {
        return Err(HpackError::Truncated);
    };
    *buf = rest;
    let huff = first & 0x80 != 0;
    let len = read_prefixed_int(buf, 7, first & 0x7f)? as usize;
    if buf.len() < len {
        return Err(HpackError::Truncated);
    }
    let raw = &buf[..len];
    *buf = &buf[len..];
    if huff {
        huffman_decode(raw)
    } else {
        Ok(raw.to_vec())
    }
}

/// Decode one header block into `(name, value)` pairs,
/// updating `table` with incremental insertions / size
/// updates as instructed by the block.
pub fn decode_header_block(
    mut buf: &[u8],
    table: &mut DynamicTable,
) -> Result<Vec<(String, Vec<u8>)>, HpackError> {
    let mut out = Vec::new();
    while let Some((&first, rest)) = buf.split_first() {
        buf = rest;
        if first & 0x80 != 0 {
            // §6.1 Indexed field.
            let idx = read_prefixed_int(&mut buf, 7, first & 0x7f)?;
            let (n, v) = table.get(idx)?;
            out.push((bytes_to_name(n)?, v.to_vec()));
        } else if first & 0x40 != 0 {
            // §6.2.1 Literal with incremental indexing.
            let idx = read_prefixed_int(&mut buf, 6, first & 0x3f)?;
            let name = if idx == 0 {
                read_string(&mut buf)?
            } else {
                table.get(idx)?.0.to_vec()
            };
            let value = read_string(&mut buf)?;
            out.push((bytes_to_name(&name)?, value.clone()));
            table.insert(name, value);
        } else if first & 0x20 != 0 {
            // §6.3 Dynamic-table size update.
            let new = read_prefixed_int(&mut buf, 5, first & 0x1f)?;
            table.set_max_size(new as usize)?;
        } else {
            // §6.2.2 / §6.2.3 Literal without indexing /
            // never-indexed (same wire shape, 4-bit prefix).
            let idx = read_prefixed_int(&mut buf, 4, first & 0x0f)?;
            let name = if idx == 0 {
                read_string(&mut buf)?
            } else {
                table.get(idx)?.0.to_vec()
            };
            let value = read_string(&mut buf)?;
            out.push((bytes_to_name(&name)?, value));
        }
    }
    Ok(out)
}

fn bytes_to_name(b: &[u8]) -> Result<String, HpackError> {
    core::str::from_utf8(b)
        .map(String::from)
        .map_err(|_| HpackError::BadHuffman)
}

// ============================================================
// Header-block encode — indexed where the static table has
// an exact match, indexed-name + literal value otherwise,
// full literal as the fallback. No Huffman, no dynamic
// insertions from our side (stateless encoder: simple and
// always legal — the peer's decoder table stays empty for
// our blocks).
// ============================================================

/// Encode a header list as one HPACK block.
pub fn encode_header_block(headers: &[(String, Vec<u8>)], out: &mut Vec<u8>) {
    for (name, value) in headers {
        let nb = name.as_bytes();
        // Exact (name, value) static match → indexed (§6.1).
        if let Some(i) = STATIC_TABLE
            .iter()
            .position(|&(n, v)| n == nb && v == value.as_slice())
        {
            write_prefixed_int((i + 1) as u64, 7, 0x80, out);
            continue;
        }
        // Name-only static match → literal w/o indexing,
        // indexed name (§6.2.2).
        if let Some(i) = STATIC_TABLE.iter().position(|&(n, _)| n == nb) {
            write_prefixed_int((i + 1) as u64, 4, 0x00, out);
        } else {
            out.push(0x00);
            write_prefixed_int(nb.len() as u64, 7, 0x00, out);
            out.extend_from_slice(nb);
        }
        write_prefixed_int(value.len() as u64, 7, 0x00, out);
        out.extend_from_slice(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_table_is_prefix_free_and_unique() {
        for (i, &(code_a, bits_a)) in HUFFMAN.iter().enumerate() {
            assert!((5..=30).contains(&bits_a), "sym {i} has odd length");
            for (j, &(code_b, bits_b)) in HUFFMAN.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    !(bits_a == bits_b && code_a == code_b),
                    "duplicate code: syms {i} and {j}"
                );
                // Prefix freedom: the shorter must not be a
                // prefix of the longer.
                let (short, slen, long, llen) = if bits_a <= bits_b {
                    (code_a, bits_a, code_b, bits_b)
                } else {
                    (code_b, bits_b, code_a, bits_a)
                };
                assert!(
                    long >> (llen - slen) != short,
                    "prefix violation between syms {i} and {j}"
                );
            }
        }
    }
}
