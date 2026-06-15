//! `.proto`-binary wire-format primitives + `ProtoCodec`
//! trait + `to_proto_bytes` / `from_proto_bytes` free
//! functions.
//!
//! See `../../REFACTOR-v8-v0.6-wire-shim.md` for scope notes
//! and the v0.6 → v0.7 split rationale. v0.6 lands the
//! primitives + trait + a hand-written demo in tests;
//! codegen-emitted `ProtoCodec` impls land at v0.7.

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================
// WireType + WireError.
// ============================================================

/// Protobuf wire types (3-bit tag suffix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    /// Variable-width integer / bool / enum.
    Varint = 0,
    /// Fixed 8-byte little-endian.
    Fixed64 = 1,
    /// Length-delimited (varint length prefix + raw bytes).
    /// Used by string / bytes / nested message / packed
    /// repeated.
    LengthDelimited = 2,
    /// Legacy `group` start delimiter (proto2). Rejected by
    /// the v0.6 decoder.
    StartGroup = 3,
    /// Legacy `group` end delimiter (proto2). Rejected by
    /// the v0.6 decoder.
    EndGroup = 4,
    /// Fixed 4-byte little-endian.
    Fixed32 = 5,
}

impl WireType {
    /// Lookup by 3-bit wire-type code. Rejects unknown
    /// wire-type codes (6 / 7) with
    /// [`WireError::UnknownWireType`].
    pub fn from_u32(n: u32) -> Result<Self, WireError> {
        match n {
            0 => Ok(WireType::Varint),
            1 => Ok(WireType::Fixed64),
            2 => Ok(WireType::LengthDelimited),
            3 => Ok(WireType::StartGroup),
            4 => Ok(WireType::EndGroup),
            5 => Ok(WireType::Fixed32),
            _ => Err(WireError::UnknownWireType(n)),
        }
    }
}

/// Byte-level failure modes raised by the wire shim. Distinct
/// from [`crate::ImportError`] because wire-level failures
/// don't carry a source line/col — they happen on opaque
/// byte buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Input ended mid-decode.
    Truncated,
    /// Varint exceeded the 10-byte (64-bit) ceiling.
    InvalidVarint,
    /// Decoded tag overflowed `u32` field-number space.
    InvalidTag,
    /// Wire-type code wasn't 0 / 1 / 2 / 3 / 4 / 5.
    UnknownWireType(u32),
    /// Wire feature recognised but not supported (e.g.
    /// `group` start/end).
    UnsupportedFeature(&'static str),
    /// String-typed field carried non-UTF-8 bytes.
    Utf8,
}

// ============================================================
// Varint (LEB128).
// ============================================================

/// Encode `value` as an unsigned LEB128 varint and append to
/// `out`. Always succeeds.
pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Decode an unsigned LEB128 varint from the start of `bytes`.
/// Returns `(value, bytes_consumed)` or [`WireError::Truncated`]
/// / [`WireError::InvalidVarint`].
pub fn decode_varint(bytes: &[u8]) -> Result<(u64, usize), WireError> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    for (i, b) in bytes.iter().take(10).copied().enumerate() {
        value |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok((value, i + 1));
        }
        shift += 7;
    }
    if bytes.len() < 10 {
        Err(WireError::Truncated)
    } else {
        Err(WireError::InvalidVarint)
    }
}

// ============================================================
// Zigzag transforms (sintN family).
// ============================================================

/// Zigzag-encode an `i32` to its unsigned wire form
/// (`(n << 1) ^ (n >> 31)` per the protobuf spec).
pub fn zigzag_encode_i32(n: i32) -> u64 {
    (((n as u32) << 1) ^ ((n >> 31) as u32)) as u64
}

/// Zigzag-decode an unsigned wire value back to `i32`.
pub fn zigzag_decode_i32(n: u64) -> i32 {
    let low = n as u32;
    ((low >> 1) as i32) ^ -((low & 1) as i32)
}

/// Zigzag-encode an `i64` to its unsigned wire form.
pub fn zigzag_encode_i64(n: i64) -> u64 {
    ((n as u64) << 1) ^ ((n >> 63) as u64)
}

/// Zigzag-decode an unsigned wire value back to `i64`.
pub fn zigzag_decode_i64(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

// ============================================================
// Fixed-width LE.
// ============================================================

/// Append `v` as 4 little-endian bytes.
pub fn encode_fixed32(v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Decode 4 LE bytes off the start of `bytes`.
pub fn decode_fixed32(bytes: &[u8]) -> Result<(u32, usize), WireError> {
    if bytes.len() < 4 {
        return Err(WireError::Truncated);
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    Ok((u32::from_le_bytes(buf), 4))
}

/// Append `v` as 8 little-endian bytes.
pub fn encode_fixed64(v: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Decode 8 LE bytes off the start of `bytes`.
pub fn decode_fixed64(bytes: &[u8]) -> Result<(u64, usize), WireError> {
    if bytes.len() < 8 {
        return Err(WireError::Truncated);
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    Ok((u64::from_le_bytes(buf), 8))
}

// ============================================================
// Tag = (field_number << 3) | wire_type.
// ============================================================

/// Encode a `(field_number, wire_type)` pair as a varint tag.
pub fn encode_tag(field_number: u32, wire_type: WireType, out: &mut Vec<u8>) {
    let tag = ((field_number as u64) << 3) | (wire_type as u64);
    encode_varint(tag, out);
}

/// Decode a tag, returning `(field_number, wire_type, bytes_consumed)`.
///
/// Rejects:
/// - Field number `0` ([`WireError::InvalidTag`]).
/// - Field number > `u32::MAX` ([`WireError::InvalidTag`]).
/// - Wire-type codes 6 / 7 ([`WireError::UnknownWireType`]).
/// - `StartGroup` / `EndGroup`
///   ([`WireError::UnsupportedFeature`]).
pub fn decode_tag(bytes: &[u8]) -> Result<(u32, WireType, usize), WireError> {
    let (raw, n) = decode_varint(bytes)?;
    let wt_code = (raw & 0x7) as u32;
    let field_number_u64 = raw >> 3;
    if field_number_u64 == 0 {
        return Err(WireError::InvalidTag);
    }
    if field_number_u64 > u32::MAX as u64 {
        return Err(WireError::InvalidTag);
    }
    let field_number = field_number_u64 as u32;
    let wire_type = WireType::from_u32(wt_code)?;
    if matches!(wire_type, WireType::StartGroup | WireType::EndGroup) {
        return Err(WireError::UnsupportedFeature(
            "wire: `group` start/end tags are not supported",
        ));
    }
    Ok((field_number, wire_type, n))
}

// ============================================================
// Length-delimited.
// ============================================================

/// Encode a length-prefixed byte payload (varint length +
/// `payload` bytes).
pub fn encode_length_delimited(payload: &[u8], out: &mut Vec<u8>) {
    encode_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// Decode a length-prefixed payload off the start of `bytes`.
/// Returns `(payload_slice, bytes_consumed)`. The payload
/// slice borrows from `bytes`.
pub fn decode_length_delimited(bytes: &[u8]) -> Result<(&[u8], usize), WireError> {
    let (len_u64, n_len) = decode_varint(bytes)?;
    let len = len_u64 as usize;
    let rest = &bytes[n_len..];
    if rest.len() < len {
        return Err(WireError::Truncated);
    }
    Ok((&rest[..len], n_len + len))
}

/// Convenience: decode a UTF-8 string from a length-delimited
/// payload. Wraps [`decode_length_delimited`] + a UTF-8
/// validity check.
pub fn decode_string(bytes: &[u8]) -> Result<(String, usize), WireError> {
    let (slice, n) = decode_length_delimited(bytes)?;
    let s = core::str::from_utf8(slice)
        .map_err(|_| WireError::Utf8)?
        .into();
    Ok((s, n))
}

// ============================================================
// ProtoCodec trait + free-function entry points.
// ============================================================

/// Round-trip protobuf wire format for a Rust type.
///
/// Implementors emit / consume the byte sequence directly;
/// the trait is intentionally minimal so codegen-emitted
/// impls (v0.7) can stay tight.
pub trait ProtoCodec: Sized {
    /// Append the protobuf wire-format encoding of `self` to
    /// `out`.
    fn encode(&self, out: &mut Vec<u8>);

    /// Decode an owned instance from `bytes`. Decoders should
    /// drop unknown fields (skipping by wire type) and ignore
    /// trailing bytes only if explicitly documented.
    fn decode(bytes: &[u8]) -> Result<Self, WireError>;
}

/// Encode `value` into a fresh `Vec<u8>`.
pub fn to_proto_bytes<T: ProtoCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// Decode a `T` from `bytes`.
pub fn from_proto_bytes<T: ProtoCodec>(bytes: &[u8]) -> Result<T, WireError> {
    T::decode(bytes)
}

/// Skip the value bytes for an unknown field whose tag was
/// already consumed. Useful for forward-compatible decoders.
pub fn skip_field(wire_type: WireType, bytes: &[u8]) -> Result<usize, WireError> {
    match wire_type {
        WireType::Varint => {
            let (_, n) = decode_varint(bytes)?;
            Ok(n)
        }
        WireType::Fixed64 => {
            if bytes.len() < 8 {
                Err(WireError::Truncated)
            } else {
                Ok(8)
            }
        }
        WireType::LengthDelimited => {
            let (_, n) = decode_length_delimited(bytes)?;
            Ok(n)
        }
        WireType::Fixed32 => {
            if bytes.len() < 4 {
                Err(WireError::Truncated)
            } else {
                Ok(4)
            }
        }
        WireType::StartGroup | WireType::EndGroup => Err(WireError::UnsupportedFeature(
            "wire: `group` start/end tags are not supported",
        )),
    }
}

// ============================================================
// Unit tests — primitives only; end-to-end ProtoCodec round-
// trip lives in `tests/kat_wire.rs`.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip_known_values() {
        for v in [0u64, 1, 127, 128, 16_383, 16_384, 0xFFFF_FFFF, u64::MAX] {
            let mut buf = Vec::new();
            encode_varint(v, &mut buf);
            let (decoded, n) = decode_varint(&buf).unwrap();
            assert_eq!(decoded, v);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn varint_encoding_widths() {
        let mut buf = Vec::new();
        encode_varint(127, &mut buf);
        assert_eq!(buf, [0x7F]);
        buf.clear();
        encode_varint(128, &mut buf);
        assert_eq!(buf, [0x80, 0x01]);
        buf.clear();
        encode_varint(300, &mut buf);
        // 300 = 0b100101100 → low 7 bits 0b0101100 = 0x2C w/
        // continuation, high 2 bits 0b10 = 0x02.
        assert_eq!(buf, [0xAC, 0x02]);
    }

    #[test]
    fn varint_truncated_rejected() {
        // Only continuation bytes — no terminator.
        let err = decode_varint(&[0x80, 0x80, 0x80]).unwrap_err();
        assert_eq!(err, WireError::Truncated);
    }

    #[test]
    fn varint_overlong_rejected() {
        // 10 continuation bytes — varint overflow (would
        // encode a value beyond 64 bits).
        let buf = [0x80u8; 10];
        let err = decode_varint(&buf).unwrap_err();
        assert_eq!(err, WireError::InvalidVarint);
    }

    #[test]
    fn zigzag_i32_round_trip() {
        for n in [0i32, 1, -1, 42, -42, i32::MAX, i32::MIN] {
            assert_eq!(zigzag_decode_i32(zigzag_encode_i32(n)), n);
        }
        assert_eq!(zigzag_encode_i32(0), 0);
        assert_eq!(zigzag_encode_i32(-1), 1);
        assert_eq!(zigzag_encode_i32(1), 2);
        assert_eq!(zigzag_encode_i32(i32::MIN), u32::MAX as u64);
    }

    #[test]
    fn zigzag_i64_round_trip() {
        for n in [0i64, 1, -1, 42, -42, i64::MAX, i64::MIN] {
            assert_eq!(zigzag_decode_i64(zigzag_encode_i64(n)), n);
        }
        assert_eq!(zigzag_encode_i64(i64::MIN), u64::MAX);
    }

    #[test]
    fn fixed32_round_trip() {
        let mut buf = Vec::new();
        encode_fixed32(0xDEAD_BEEF, &mut buf);
        assert_eq!(buf, [0xEF, 0xBE, 0xAD, 0xDE]);
        let (v, n) = decode_fixed32(&buf).unwrap();
        assert_eq!(v, 0xDEAD_BEEF);
        assert_eq!(n, 4);
    }

    #[test]
    fn fixed64_round_trip() {
        let mut buf = Vec::new();
        encode_fixed64(0x0123_4567_89AB_CDEF, &mut buf);
        let (v, n) = decode_fixed64(&buf).unwrap();
        assert_eq!(v, 0x0123_4567_89AB_CDEF);
        assert_eq!(n, 8);
    }

    #[test]
    fn fixed_truncated() {
        assert_eq!(
            decode_fixed32(&[1, 2, 3]).unwrap_err(),
            WireError::Truncated
        );
        assert_eq!(
            decode_fixed64(&[1, 2, 3, 4, 5, 6, 7]).unwrap_err(),
            WireError::Truncated
        );
    }

    #[test]
    fn tag_encode_known_bytes() {
        // Field 1, wire-type Varint → 0b00001000 = 0x08.
        let mut buf = Vec::new();
        encode_tag(1, WireType::Varint, &mut buf);
        assert_eq!(buf, [0x08]);
        // Field 2, wire-type LengthDelimited → 0b00010010 = 0x12.
        buf.clear();
        encode_tag(2, WireType::LengthDelimited, &mut buf);
        assert_eq!(buf, [0x12]);
        // Field 15, wire-type Fixed32 → 0b01111101 = 0x7D.
        buf.clear();
        encode_tag(15, WireType::Fixed32, &mut buf);
        assert_eq!(buf, [0x7D]);
        // Field 2048 (multi-byte varint), wire-type Fixed64.
        buf.clear();
        encode_tag(2048, WireType::Fixed64, &mut buf);
        // (2048 << 3) | 1 = 0b100000000000_001 = 16385 → varint
        // 0x81 0x80 0x01.
        assert_eq!(buf, [0x81, 0x80, 0x01]);
    }

    #[test]
    fn tag_decode_round_trip() {
        let cases = [
            (1u32, WireType::Varint),
            (2, WireType::LengthDelimited),
            (15, WireType::Fixed32),
            (2048, WireType::Fixed64),
            (536_870_911, WireType::Varint),
        ];
        for (fld, wt) in cases {
            let mut buf = Vec::new();
            encode_tag(fld, wt, &mut buf);
            let (dec_fld, dec_wt, _) = decode_tag(&buf).unwrap();
            assert_eq!((dec_fld, dec_wt), (fld, wt));
        }
    }

    #[test]
    fn tag_rejects_zero_field() {
        // Tag varint = 0 → field 0, wire-type Varint. Field 0
        // is illegal per spec.
        let err = decode_tag(&[0x00]).unwrap_err();
        assert_eq!(err, WireError::InvalidTag);
    }

    #[test]
    fn tag_rejects_group_wire_types() {
        // Field 1, wire-type StartGroup → 0b00001011 = 0x0B.
        let err = decode_tag(&[0x0B]).unwrap_err();
        assert!(matches!(err, WireError::UnsupportedFeature(_)));
        let err = decode_tag(&[0x0C]).unwrap_err();
        assert!(matches!(err, WireError::UnsupportedFeature(_)));
    }

    #[test]
    fn tag_rejects_unknown_wire_types() {
        // Wire-type code 6 / 7 are reserved/unused.
        let err = decode_tag(&[0x0E]).unwrap_err(); // field 1, wt 6
        assert!(matches!(err, WireError::UnknownWireType(6)));
        let err = decode_tag(&[0x0F]).unwrap_err(); // field 1, wt 7
        assert!(matches!(err, WireError::UnknownWireType(7)));
    }

    #[test]
    fn length_delimited_round_trip() {
        let mut buf = Vec::new();
        encode_length_delimited(b"hello", &mut buf);
        // Length=5 → varint 0x05, then "hello".
        assert_eq!(buf, [0x05, b'h', b'e', b'l', b'l', b'o']);
        let (slice, n) = decode_length_delimited(&buf).unwrap();
        assert_eq!(slice, b"hello");
        assert_eq!(n, buf.len());
    }

    #[test]
    fn length_delimited_empty() {
        let mut buf = Vec::new();
        encode_length_delimited(b"", &mut buf);
        assert_eq!(buf, [0x00]);
        let (slice, n) = decode_length_delimited(&buf).unwrap();
        assert!(slice.is_empty());
        assert_eq!(n, 1);
    }

    #[test]
    fn length_delimited_truncated() {
        // Length 5 but only 3 bytes follow.
        let err = decode_length_delimited(&[0x05, b'h', b'i', b'!']).unwrap_err();
        assert_eq!(err, WireError::Truncated);
    }

    #[test]
    fn decode_string_rejects_non_utf8() {
        // length=2, payload = 0xFF 0xFE — invalid UTF-8.
        let bytes = [0x02, 0xFF, 0xFE];
        let err = decode_string(&bytes).unwrap_err();
        assert_eq!(err, WireError::Utf8);
    }

    #[test]
    fn skip_field_varint() {
        // Varint of value 300 = [0xAC, 0x02]; skip should
        // consume 2 bytes.
        let bytes = [0xAC, 0x02, 0xFF];
        let n = skip_field(WireType::Varint, &bytes).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn skip_field_length_delimited() {
        let bytes = [0x03, 0xAA, 0xBB, 0xCC, 0xFF];
        let n = skip_field(WireType::LengthDelimited, &bytes).unwrap();
        assert_eq!(n, 4);
    }
}
