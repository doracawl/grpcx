//! RFC 9297 capsule framing — the wire shape that rides on top
//! of the bootstrap H/3 request stream once a WebTransport
//! session is established. Currently used only for
//! `CLOSE_WEBTRANSPORT_SESSION` (RFC 9220 §4.4); v1.x adds
//! `DRAIN_WEBTRANSPORT_SESSION` (§4.5) once we model drain
//! semantics.
//!
//! Wire shape (RFC 9297 §3.2):
//! `[capsule_type varint][capsule_length varint][payload]`
//!
//! `CLOSE_WEBTRANSPORT_SESSION` payload (RFC 9220 §4.4):
//! `[error_code u32 big-endian][reason bytes (≤ 1024)]`

use crate::Error;
use crate::varint::{read_varint, varint_len, write_varint};
use alloc::vec::Vec;

/// RFC 9220 §4.4 capsule type.
pub const CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION: u64 = 0x2843;

/// RFC 9220 §5.3 — reason bounded so it can't bloat the stream.
const MAX_CLOSE_REASON_LEN: usize = 1024;

/// One parsed capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capsule {
    /// RFC 9220 §4.4 — graceful session shutdown signal.
    Close {
        /// Application-defined error code.
        code: u32,
        /// Optional human-readable reason.
        reason: Vec<u8>,
    },
    /// RFC 9297 §3.3 — forward-compat: unknown capsule types
    /// MUST be skipped (length carries the skip span).
    Unknown {
        /// Capsule type code from the leading varint.
        type_: u64,
        /// Payload bytes the caller can ignore.
        payload: Vec<u8>,
    },
}

/// Emit a `CLOSE_WEBTRANSPORT_SESSION` capsule into `out`.
/// Rejects with `Error::BadStreamFrame` if `reason` is longer
/// than 1024 bytes (RFC 9220 §5.3).
pub fn encode_close_capsule(code: u32, reason: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    if reason.len() > MAX_CLOSE_REASON_LEN {
        return Err(Error::BadStreamFrame);
    }
    // payload = [u32 BE code][reason]
    let payload_len = 4 + reason.len();
    put_varint(CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION, out)?;
    put_varint(payload_len as u64, out)?;
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(reason);
    Ok(())
}

/// Try to peel one capsule off the front of `buf`.
///
/// - `Ok(None)` — truncated (varint or payload incomplete);
///   caller waits for more bytes.
/// - `Ok(Some(_))` — one capsule parsed; `*buf` is advanced
///   past it.
/// - `Err(Error::BadStreamFrame)` — payload length doesn't
///   match the wire format (e.g. `CLOSE` with a payload < 4
///   bytes for the u32 code, or a `reason` over the
///   1024-byte cap).
pub fn decode_capsule(buf: &mut &[u8]) -> Result<Option<Capsule>, Error> {
    let mut cursor: &[u8] = buf;
    // Peek the type varint.
    let Ok(type_) = read_varint(&mut cursor) else {
        return Ok(None);
    };
    let Ok(length) = read_varint(&mut cursor) else {
        return Ok(None);
    };
    if cursor.len() < length as usize {
        // payload not fully arrived
        return Ok(None);
    }
    let payload = &cursor[..length as usize];
    let rest = &cursor[length as usize..];
    let capsule = match type_ {
        CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION => {
            if payload.len() < 4 {
                return Err(Error::BadStreamFrame);
            }
            let reason_len = payload.len() - 4;
            if reason_len > MAX_CLOSE_REASON_LEN {
                return Err(Error::BadStreamFrame);
            }
            let code = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let reason = payload[4..].to_vec();
            Capsule::Close { code, reason }
        }
        _ => Capsule::Unknown {
            type_,
            payload: payload.to_vec(),
        },
    };
    *buf = rest;
    Ok(Some(capsule))
}

/// Encoded wire length of a capsule with payload of `payload_len`
/// bytes. Useful for stream-buffer sizing.
pub fn capsule_wire_len(type_: u64, payload_len: usize) -> usize {
    varint_len(type_) + varint_len(payload_len as u64) + payload_len
}

fn put_varint(value: u64, out: &mut Vec<u8>) -> Result<(), Error> {
    let n = varint_len(value);
    let mut buf = [0u8; 8];
    write_varint(value, &mut buf)?;
    out.extend_from_slice(&buf[..n]);
    Ok(())
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_round_trip_empty_reason() {
        let mut out = Vec::new();
        encode_close_capsule(0x12345678, b"", &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        let cap = decode_capsule(&mut cursor).unwrap().unwrap();
        assert_eq!(
            cap,
            Capsule::Close {
                code: 0x12345678,
                reason: Vec::new(),
            }
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn close_round_trip_with_reason() {
        let mut out = Vec::new();
        encode_close_capsule(7, b"oops", &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        let cap = decode_capsule(&mut cursor).unwrap().unwrap();
        assert_eq!(
            cap,
            Capsule::Close {
                code: 7,
                reason: b"oops".to_vec()
            }
        );
    }

    #[test]
    fn truncated_returns_none() {
        let mut out = Vec::new();
        encode_close_capsule(1, b"hi", &mut out).unwrap();
        for n in 0..out.len() {
            let mut cursor: &[u8] = &out[..n];
            assert!(decode_capsule(&mut cursor).unwrap().is_none());
        }
    }

    #[test]
    fn unknown_capsule_type_forward_compat() {
        // Hand-craft a capsule with type = 0xFE (unknown) and a
        // 3-byte payload.
        let mut out = Vec::new();
        // Type 0xFE encodes as a 2-byte varint (0x40 0xFE).
        out.push(0x40);
        out.push(0xFE);
        out.push(3); // length
        out.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        let mut cursor: &[u8] = &out;
        let cap = decode_capsule(&mut cursor).unwrap().unwrap();
        assert_eq!(
            cap,
            Capsule::Unknown {
                type_: 0xFE,
                payload: vec![0xaa, 0xbb, 0xcc]
            }
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn encode_oversize_reason_rejected() {
        let big = vec![b'x'; MAX_CLOSE_REASON_LEN + 1];
        let mut out = Vec::new();
        assert_eq!(
            encode_close_capsule(0, &big, &mut out),
            Err(Error::BadStreamFrame)
        );
    }

    #[test]
    fn close_payload_under_4_bytes_rejected() {
        // type=0x2843 (varint 0x68 0x43), length=2, payload [0xaa, 0xbb].
        let out: Vec<u8> = vec![0x68, 0x43, 0x02, 0xaa, 0xbb];
        let mut cursor: &[u8] = &out;
        assert_eq!(decode_capsule(&mut cursor), Err(Error::BadStreamFrame));
    }

    #[test]
    fn capsule_wire_len_matches_encode() {
        let mut out = Vec::new();
        encode_close_capsule(5, b"hello", &mut out).unwrap();
        assert_eq!(
            out.len(),
            capsule_wire_len(CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION, 4 + 5),
        );
    }

    #[test]
    fn two_capsules_back_to_back() {
        let mut out = Vec::new();
        encode_close_capsule(1, b"a", &mut out).unwrap();
        encode_close_capsule(2, b"bb", &mut out).unwrap();
        let mut cursor: &[u8] = &out;
        let c1 = decode_capsule(&mut cursor).unwrap().unwrap();
        let c2 = decode_capsule(&mut cursor).unwrap().unwrap();
        assert_eq!(
            c1,
            Capsule::Close {
                code: 1,
                reason: b"a".to_vec()
            }
        );
        assert_eq!(
            c2,
            Capsule::Close {
                code: 2,
                reason: b"bb".to_vec()
            }
        );
        assert!(cursor.is_empty());
    }
}
