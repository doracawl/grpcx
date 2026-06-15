//! QPACK static-only encoder + decoder — RFC 9204.
//!
//! v0.3 ships the static-table path:
//! - Full 99-entry static table (RFC 9204 Appendix A).
//! - Prefixed-integer codec (§4.1.1).
//! - Encoded Field Section prefix in its static-only fixed form
//!   (Required Insert Count = 0, Sign = 0, Delta Base = 0).
//! - Three field-line patterns: Indexed (§4.5.2 with T = static),
//!   Literal with Name Reference (§4.5.4 with T = static), and
//!   Literal with Literal Name (§4.5.6).
//!
//! Out of scope for v0.3 (defers per `docs/v5-scope.md`):
//! - Dynamic table (v0.6) — and with it, encoder / decoder
//!   instruction streams.
//! - Post-Base patterns (§4.5.3 / §4.5.5).
//! - Huffman emit (encoder always emits `H=0`); the decoder
//!   rejects `H=1` with `QpackDecode` — Huffman decode lands in
//!   v1.x perf pass alongside the encoder switch.

use crate::{Error, Header};
use alloc::vec::Vec;

// ============================================================
// Static table — RFC 9204 Appendix A, 99 entries (0..=98).
// ============================================================

/// (name, value) pairs of the QPACK static table.
const STATIC_TABLE: &[(&[u8], &[u8])] = &[
    (b":authority", b""),
    (b":path", b"/"),
    (b"age", b"0"),
    (b"content-disposition", b""),
    (b"content-length", b"0"),
    (b"cookie", b""),
    (b"date", b""),
    (b"etag", b""),
    (b"if-modified-since", b""),
    (b"if-none-match", b""),
    (b"last-modified", b""),
    (b"link", b""),
    (b"location", b""),
    (b"referer", b""),
    (b"set-cookie", b""),
    (b":method", b"CONNECT"),
    (b":method", b"DELETE"),
    (b":method", b"GET"),
    (b":method", b"HEAD"),
    (b":method", b"OPTIONS"),
    (b":method", b"POST"),
    (b":method", b"PUT"),
    (b":scheme", b"http"),
    (b":scheme", b"https"),
    (b":status", b"103"),
    (b":status", b"200"),
    (b":status", b"304"),
    (b":status", b"404"),
    (b":status", b"503"),
    (b"accept", b"*/*"),
    (b"accept", b"application/dns-message"),
    (b"accept-encoding", b"gzip, deflate, br"),
    (b"accept-ranges", b"bytes"),
    (b"access-control-allow-headers", b"cache-control"),
    (b"access-control-allow-headers", b"content-type"),
    (b"access-control-allow-origin", b"*"),
    (b"cache-control", b"max-age=0"),
    (b"cache-control", b"max-age=2592000"),
    (b"cache-control", b"max-age=604800"),
    (b"cache-control", b"no-cache"),
    (b"cache-control", b"no-store"),
    (b"cache-control", b"public, max-age=31536000"),
    (b"content-encoding", b"br"),
    (b"content-encoding", b"gzip"),
    (b"content-type", b"application/dns-message"),
    (b"content-type", b"application/javascript"),
    (b"content-type", b"application/json"),
    (b"content-type", b"application/x-www-form-urlencoded"),
    (b"content-type", b"image/gif"),
    (b"content-type", b"image/jpeg"),
    (b"content-type", b"image/png"),
    (b"content-type", b"text/css"),
    (b"content-type", b"text/html; charset=utf-8"),
    (b"content-type", b"text/plain"),
    (b"content-type", b"text/plain;charset=utf-8"),
    (b"range", b"bytes=0-"),
    (b"strict-transport-security", b"max-age=31536000"),
    (
        b"strict-transport-security",
        b"max-age=31536000; includesubdomains",
    ),
    (
        b"strict-transport-security",
        b"max-age=31536000; includesubdomains; preload",
    ),
    (b"vary", b"accept-encoding"),
    (b"vary", b"origin"),
    (b"x-content-type-options", b"nosniff"),
    (b"x-xss-protection", b"1; mode=block"),
    (b":status", b"100"),
    (b":status", b"204"),
    (b":status", b"206"),
    (b":status", b"302"),
    (b":status", b"400"),
    (b":status", b"403"),
    (b":status", b"421"),
    (b":status", b"425"),
    (b":status", b"500"),
    (b"accept-language", b""),
    (b"access-control-allow-credentials", b"FALSE"),
    (b"access-control-allow-credentials", b"TRUE"),
    (b"access-control-allow-headers", b"*"),
    (b"access-control-allow-methods", b"get"),
    (b"access-control-allow-methods", b"get, post, options"),
    (b"access-control-allow-methods", b"options"),
    (b"access-control-expose-headers", b"content-length"),
    (b"access-control-request-headers", b"content-type"),
    (b"access-control-request-method", b"get"),
    (b"access-control-request-method", b"post"),
    (b"alt-svc", b"clear"),
    (b"authorization", b""),
    (
        b"content-security-policy",
        b"script-src 'none'; object-src 'none'; base-uri 'none'",
    ),
    (b"early-data", b"1"),
    (b"expect-ct", b""),
    (b"forwarded", b""),
    (b"if-range", b""),
    (b"origin", b""),
    (b"purpose", b"prefetch"),
    (b"server", b""),
    (b"timing-allow-origin", b"*"),
    (b"upgrade-insecure-requests", b"1"),
    (b"user-agent", b""),
    (b"x-forwarded-for", b""),
    (b"x-frame-options", b"deny"),
    (b"x-frame-options", b"sameorigin"),
];

/// Look up a static-table entry by index (0..=98).
pub fn static_entry(index: u64) -> Option<(&'static [u8], &'static [u8])> {
    STATIC_TABLE.get(index as usize).copied()
}

/// Return the static-table index of the exact `(name, value)`
/// match, if any.
pub fn find_static(name: &[u8], value: &[u8]) -> Option<u64> {
    STATIC_TABLE.iter().enumerate().find_map(|(i, (n, v))| {
        if *n == name && *v == value {
            Some(i as u64)
        } else {
            None
        }
    })
}

/// Return the **first** static-table index whose name matches
/// `name`, regardless of value. Used for emitting Literal-with-
/// Name-Reference when the value isn't a static hit.
pub fn find_static_name(name: &[u8]) -> Option<u64> {
    STATIC_TABLE
        .iter()
        .enumerate()
        .find_map(|(i, (n, _))| if *n == name { Some(i as u64) } else { None })
}

// ============================================================
// Prefixed-integer codec — RFC 9204 §4.1.1.
// ============================================================

/// Decode a prefixed integer from `buf`. `prefix_bits` ∈ 1..=8.
/// The first byte's low `prefix_bits` carry the leading part;
/// the upper `8 - prefix_bits` bits are flags the caller has
/// already inspected and must mask off via `first_byte_value`.
///
/// `first_byte_value` is the masked low-`prefix_bits` of `buf[0]`
/// (caller-supplied to avoid re-reading); `buf` is advanced past
/// the consumed bytes.
pub(crate) fn read_prefixed_int(
    buf: &mut &[u8],
    prefix_bits: u8,
    first_byte_value: u8,
) -> Result<u64, Error> {
    debug_assert!((1..=8).contains(&prefix_bits));
    // `1u8 << 8` would overflow; widen and downcast.
    let max_prefix: u8 = ((1u16 << prefix_bits) - 1) as u8;
    if first_byte_value < max_prefix {
        return Ok(first_byte_value as u64);
    }
    let mut value: u64 = max_prefix as u64;
    let mut shift: u32 = 0;
    loop {
        let &[byte, ref rest @ ..] = *buf else {
            return Err(Error::QpackDecode);
        };
        *buf = rest;
        let chunk = (byte & 0x7f) as u64;
        // Guard against shift overflow: prefixed integers must
        // fit in u64; QPACK's varint chain caps at ~10 bytes.
        if shift >= 64 {
            return Err(Error::QpackDecode);
        }
        value = value
            .checked_add(chunk.checked_shl(shift).ok_or(Error::QpackDecode)?)
            .ok_or(Error::QpackDecode)?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

/// Encode a prefixed integer into `out`. `flags` are the
/// high `8 - prefix_bits` bits of the first byte; the prefix
/// integer occupies the low `prefix_bits`.
pub(crate) fn write_prefixed_int(
    value: u64,
    prefix_bits: u8,
    flags: u8,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    debug_assert!((1..=8).contains(&prefix_bits));
    let max_prefix = (1u64 << prefix_bits) - 1;
    if value < max_prefix {
        out.push(flags | (value as u8));
        return Ok(());
    }
    out.push(flags | (max_prefix as u8));
    let mut rest = value - max_prefix;
    while rest >= 0x80 {
        out.push(((rest & 0x7f) as u8) | 0x80);
        rest >>= 7;
    }
    out.push(rest as u8);
    Ok(())
}

// ============================================================
// String codec — H=0 only on the encode side. H=1 on decode is
// rejected (v0.3 deferral; v1.x adds Huffman).
// ============================================================

pub(crate) fn read_string(
    buf: &mut &[u8],
    huffman_bit_mask: u8,
    prefix_bits: u8,
    first_byte_value: u8,
) -> Result<Vec<u8>, Error> {
    let is_huffman = (first_byte_value & huffman_bit_mask) != 0;
    if is_huffman {
        return Err(Error::QpackDecode);
    }
    // Strip the Huffman bit from the value so the integer
    // decode sees only the length-prefix bits.
    let length_prefix_value = first_byte_value & !huffman_bit_mask;
    let length = read_prefixed_int(buf, prefix_bits, length_prefix_value)? as usize;
    if buf.len() < length {
        return Err(Error::QpackDecode);
    }
    let (head, tail) = buf.split_at(length);
    *buf = tail;
    Ok(head.to_vec())
}

pub(crate) fn write_string(
    s: &[u8],
    prefix_bits: u8,
    flags: u8,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    // H = 0 (plain).
    write_prefixed_int(s.len() as u64, prefix_bits, flags, out)?;
    out.extend_from_slice(s);
    Ok(())
}

// ============================================================
// Field-section encode.
// ============================================================

/// Encode `headers` as an Encoded Field Section (RFC 9204 §4.5).
/// Always emits the static-only prefix `(RIC=0, S=0, Base=0)`
/// followed by one field line per header. Static hits become
/// Indexed Field Lines; name-only hits become Literal w/ Name
/// Reference; everything else becomes Literal w/ Literal Name.
pub fn encode_field_section(headers: &[Header], out: &mut Vec<u8>) -> Result<(), Error> {
    // Section prefix: Required Insert Count (8-bit prefix int,
    // encoded as 0 here since we never reference the dynamic
    // table), then Sign|Delta-Base (1 bit + 7-bit prefix int,
    // both zero for static-only).
    write_prefixed_int(0, 8, 0, out)?;
    write_prefixed_int(0, 7, 0, out)?;

    for h in headers {
        encode_one_field(h, out)?;
    }
    Ok(())
}

fn encode_one_field(h: &Header, out: &mut Vec<u8>) -> Result<(), Error> {
    // Try a full `(name, value)` static hit first.
    if let Some(idx) = find_static(&h.name, &h.value) {
        // Indexed Field Line — `1 T iiiiii`. T = 1 for static.
        // 6-bit prefix integer for the index.
        write_prefixed_int(idx, 6, 0b1100_0000, out)?;
        return Ok(());
    }
    // Name-only hit → Literal Field Line With Name Reference.
    if let Some(name_idx) = find_static_name(&h.name) {
        // First byte: `0 1 N T iiii`. N = 0 (not never-indexed),
        // T = 1 (static), 4-bit prefix integer for the name index.
        write_prefixed_int(name_idx, 4, 0b0101_0000, out)?;
        // Value string: `H xxxxxxx` with H = 0; 7-bit prefix.
        write_string(&h.value, 7, 0b0000_0000, out)?;
        return Ok(());
    }
    // No static hit at all → Literal Field Line With Literal Name.
    // First byte: `0 0 1 N H nnn`. N = 0, H = 0; 3-bit prefix
    // integer for the name length.
    write_string(&h.name, 3, 0b0010_0000, out)?;
    // Value string: `H xxxxxxx` with H = 0; 7-bit prefix.
    write_string(&h.value, 7, 0b0000_0000, out)?;
    Ok(())
}

// ============================================================
// Field-section decode.
// ============================================================

/// Decode an Encoded Field Section. Rejects any prefix shape
/// that depends on the dynamic table (non-zero Required Insert
/// Count or non-zero Delta Base).
pub fn decode_field_section(buf: &[u8]) -> Result<Vec<Header>, Error> {
    let mut cursor = buf;
    // Required Insert Count (8-bit prefix integer).
    if cursor.is_empty() {
        return Err(Error::QpackDecode);
    }
    let ric_first = cursor[0];
    cursor = &cursor[1..];
    let ric = read_prefixed_int(&mut cursor, 8, ric_first)?;
    if ric != 0 {
        return Err(Error::QpackDecode);
    }
    // Sign + Delta Base.
    if cursor.is_empty() {
        return Err(Error::QpackDecode);
    }
    let sb_first = cursor[0];
    cursor = &cursor[1..];
    let sign = sb_first & 0b1000_0000 != 0;
    let delta_base = read_prefixed_int(&mut cursor, 7, sb_first & 0b0111_1111)?;
    if sign || delta_base != 0 {
        return Err(Error::QpackDecode);
    }

    let mut headers = Vec::new();
    while !cursor.is_empty() {
        headers.push(decode_one_field(&mut cursor)?);
    }
    Ok(headers)
}

fn decode_one_field(cursor: &mut &[u8]) -> Result<Header, Error> {
    let first = cursor[0];
    *cursor = &cursor[1..];

    if first & 0b1000_0000 != 0 {
        // Indexed Field Line: `1 T iiiiii`.
        let table_is_static = first & 0b0100_0000 != 0;
        let index = read_prefixed_int(cursor, 6, first & 0b0011_1111)?;
        if !table_is_static {
            // Dynamic-table reference — would require a non-zero
            // RIC, which we already rejected. Defence-in-depth.
            return Err(Error::QpackDecode);
        }
        let (name, value) = static_entry(index).ok_or(Error::QpackDecode)?;
        return Ok(Header {
            name: name.to_vec(),
            value: value.to_vec(),
        });
    }
    if first & 0b0100_0000 != 0 {
        // Literal Field Line With Name Reference: `0 1 N T iiii`.
        let table_is_static = first & 0b0001_0000 != 0;
        let name_index = read_prefixed_int(cursor, 4, first & 0b0000_1111)?;
        if !table_is_static {
            return Err(Error::QpackDecode);
        }
        let (name, _) = static_entry(name_index).ok_or(Error::QpackDecode)?;
        // Value: `H xxxxxxx` — 7-bit prefix length.
        if cursor.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = cursor[0];
        *cursor = &cursor[1..];
        let value = read_string(cursor, 0b1000_0000, 7, v_first)?;
        return Ok(Header {
            name: name.to_vec(),
            value,
        });
    }
    if first & 0b0010_0000 != 0 {
        // Literal Field Line With Literal Name: `0 0 1 N H nnn`.
        let name = read_string(cursor, 0b0000_1000, 3, first & 0b0000_0111)?;
        if cursor.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = cursor[0];
        *cursor = &cursor[1..];
        let value = read_string(cursor, 0b1000_0000, 7, v_first)?;
        return Ok(Header { name, value });
    }
    // Bits 7-5 are `0 0 0 ...` — that's Indexed Field Line With
    // Post-Base Index (`0001`) or Literal With Post-Base Name
    // Reference (`0000`). Both need a non-zero Base; we rejected
    // those at the prefix.
    Err(Error::QpackDecode)
}

// ============================================================
// Dynamic-aware field-section decode (v0.7).
// ============================================================

use crate::qpack_dynamic::DynamicTable;

/// Decode an Encoded Field Section against a populated
/// [`DynamicTable`]. Handles all five field-line patterns from
/// RFC 9204 §4.5:
/// - Indexed Field Line (static + dynamic, §4.5.2)
/// - Indexed Field Line With Post-Base Index (§4.5.3)
/// - Literal Field Line With Name Reference (static + dynamic,
///   §4.5.4)
/// - Literal Field Line With Post-Base Name Reference (§4.5.5)
/// - Literal Field Line With Literal Name (§4.5.6)
///
/// The Section Prefix's Required Insert Count is taken as the
/// raw 8-bit-prefix value (the v0.6+ scope skips RFC 9204
/// §4.5.1.1's wrap-around encoding; both peers in our test
/// suite use raw RIC, which matches the protocol's lower bound
/// of the wrap-around form). Section Acknowledgement /
/// Insert Count Increment accounting still works correctly with
/// raw RIC for any `actual_ric < 2 * MaxEntries`.
///
/// Returns `Error::QpackDecode` if a referenced entry has been
/// evicted, the RIC exceeds `table.insert_count()`, or the
/// section bytes are malformed.
pub fn decode_field_section_with_table(
    buf: &[u8],
    table: &DynamicTable,
) -> Result<Vec<Header>, Error> {
    let mut cursor = buf;
    // Required Insert Count (8-bit prefix integer; raw value).
    if cursor.is_empty() {
        return Err(Error::QpackDecode);
    }
    let ric_first = cursor[0];
    cursor = &cursor[1..];
    let ric = read_prefixed_int(&mut cursor, 8, ric_first)?;

    // RIC must not exceed the decoder's view of the encoder's
    // insert count — otherwise the section references entries
    // we haven't seen yet ("blocked-streams" condition; v0.7
    // doesn't support blocking, so reject).
    if ric > table.insert_count() {
        return Err(Error::QpackDecode);
    }

    // Sign + Delta Base.
    if cursor.is_empty() {
        return Err(Error::QpackDecode);
    }
    let sb_first = cursor[0];
    cursor = &cursor[1..];
    let sign = sb_first & 0b1000_0000 != 0;
    let delta_base = read_prefixed_int(&mut cursor, 7, sb_first & 0b0111_1111)?;
    // Base = RIC + DeltaBase if S=0, or RIC - DeltaBase - 1 if S=1.
    let base = if sign {
        if delta_base + 1 > ric {
            return Err(Error::QpackDecode);
        }
        ric - delta_base - 1
    } else {
        ric.checked_add(delta_base).ok_or(Error::QpackDecode)?
    };

    let mut headers = Vec::new();
    while !cursor.is_empty() {
        headers.push(decode_one_field_with_table(&mut cursor, base, table)?);
    }
    Ok(headers)
}

fn decode_one_field_with_table(
    cursor: &mut &[u8],
    base: u64,
    table: &DynamicTable,
) -> Result<Header, Error> {
    let first = cursor[0];
    *cursor = &cursor[1..];

    // Indexed Field Line: `1 T iiiiii`.
    if first & 0b1000_0000 != 0 {
        let table_is_static = first & 0b0100_0000 != 0;
        let index = read_prefixed_int(cursor, 6, first & 0b0011_1111)?;
        let (name, value) = if table_is_static {
            let (n, v) = static_entry(index).ok_or(Error::QpackDecode)?;
            (n.to_vec(), v.to_vec())
        } else {
            // Dynamic ref: absolute index = base - 1 - rel_index.
            let abs = base
                .checked_sub(1)
                .and_then(|b| b.checked_sub(index))
                .ok_or(Error::QpackDecode)?;
            let entry = table.get_absolute(abs).ok_or(Error::QpackDecode)?;
            (entry.name.clone(), entry.value.clone())
        };
        return Ok(Header { name, value });
    }
    // Literal Field Line With Name Reference: `0 1 N T iiii`.
    if first & 0b0100_0000 != 0 {
        let table_is_static = first & 0b0001_0000 != 0;
        let name_index = read_prefixed_int(cursor, 4, first & 0b0000_1111)?;
        let name = if table_is_static {
            let (n, _) = static_entry(name_index).ok_or(Error::QpackDecode)?;
            n.to_vec()
        } else {
            let abs = base
                .checked_sub(1)
                .and_then(|b| b.checked_sub(name_index))
                .ok_or(Error::QpackDecode)?;
            table
                .get_absolute(abs)
                .ok_or(Error::QpackDecode)?
                .name
                .clone()
        };
        if cursor.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = cursor[0];
        *cursor = &cursor[1..];
        let value = read_string(cursor, 0b1000_0000, 7, v_first)?;
        return Ok(Header { name, value });
    }
    // Literal Field Line With Literal Name: `0 0 1 N H nnn`.
    if first & 0b0010_0000 != 0 {
        let name = read_string(cursor, 0b0000_1000, 3, first & 0b0000_0111)?;
        if cursor.is_empty() {
            return Err(Error::QpackDecode);
        }
        let v_first = cursor[0];
        *cursor = &cursor[1..];
        let value = read_string(cursor, 0b1000_0000, 7, v_first)?;
        return Ok(Header { name, value });
    }
    // Indexed Field Line With Post-Base Index: `0 0 0 1 iiii`.
    if first & 0b0001_0000 != 0 {
        let pb_index = read_prefixed_int(cursor, 4, first & 0b0000_1111)?;
        // Absolute = base + post_base_index.
        let abs = base.checked_add(pb_index).ok_or(Error::QpackDecode)?;
        let entry = table.get_absolute(abs).ok_or(Error::QpackDecode)?;
        return Ok(Header {
            name: entry.name.clone(),
            value: entry.value.clone(),
        });
    }
    // Literal Field Line With Post-Base Name Reference: `0 0 0 0 N H iii`.
    // Bits 7-4 are `0000`. N is bit 3; H is the high bit of the
    // name-length sub-field via the 3-bit prefix int container.
    // Actually RFC 9204 §4.5.5: `0 0 0 0 N iii` for the first byte,
    // with N at bit 3 and a 3-bit prefix integer (bits 2-0). The
    // string-length H bit follows on the next varint container.
    let name_index = read_prefixed_int(cursor, 3, first & 0b0000_0111)?;
    let abs = base.checked_add(name_index).ok_or(Error::QpackDecode)?;
    let name = table
        .get_absolute(abs)
        .ok_or(Error::QpackDecode)?
        .name
        .clone();
    if cursor.is_empty() {
        return Err(Error::QpackDecode);
    }
    let v_first = cursor[0];
    *cursor = &cursor[1..];
    let value = read_string(cursor, 0b1000_0000, 7, v_first)?;
    Ok(Header { name, value })
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_int_round_trip_small() {
        let mut out = Vec::new();
        write_prefixed_int(5, 5, 0, &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        let first = cursor[0];
        cursor = &cursor[1..];
        assert_eq!(
            read_prefixed_int(&mut cursor, 5, first & 0b0001_1111).unwrap(),
            5
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn prefixed_int_round_trip_large() {
        let mut out = Vec::new();
        write_prefixed_int(1337, 5, 0, &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        let first = cursor[0];
        cursor = &cursor[1..];
        assert_eq!(
            read_prefixed_int(&mut cursor, 5, first & 0b0001_1111).unwrap(),
            1337
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn prefixed_int_round_trip_max_prefix_value() {
        // value = 2^N - 1 takes the prefix slot exactly + zero
        // continuation bytes (RFC 9204 §4.1.1 — value equal to
        // 2^N - 1 always escapes to the multi-byte form, so
        // expect 1 byte of prefix + 1 byte of continuation = 2
        // bytes).
        let mut out = Vec::new();
        write_prefixed_int(31, 5, 0, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        let mut cursor: &[u8] = &out;
        let first = cursor[0];
        cursor = &cursor[1..];
        assert_eq!(
            read_prefixed_int(&mut cursor, 5, first & 0b0001_1111).unwrap(),
            31
        );
    }

    #[test]
    fn static_lookup_smoke() {
        assert_eq!(static_entry(17), Some((&b":method"[..], &b"GET"[..])));
        assert_eq!(static_entry(25), Some((&b":status"[..], &b"200"[..])));
        assert_eq!(static_entry(99), None);
        assert_eq!(find_static(b":method", b"GET"), Some(17));
        assert_eq!(find_static(b":method", b"NOPE"), None);
        // find_static_name returns the first match.
        assert_eq!(find_static_name(b":method"), Some(15));
    }
}
