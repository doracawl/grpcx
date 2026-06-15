//! HTTP/3 unidirectional-stream type discriminator — RFC 9114 §6.2.
//!
//! Every uni-stream's first bytes are a varint identifying its
//! role. Bidirectional streams are unconditionally request
//! streams and carry no type byte. v0.4 ships the wire codec +
//! classifier; v0.5's Connection state machine actually opens /
//! dispatches the streams.

use crate::Error;
use crate::varint::{read_varint, varint_len, write_varint};
use alloc::vec::Vec;

// ============================================================
// Stream-type codes — RFC 9114 §6.2 + IANA registry.
// ============================================================

const TYPE_CONTROL: u64 = 0x00;
const TYPE_PUSH: u64 = 0x01;
const TYPE_QPACK_ENCODER: u64 = 0x02;
const TYPE_QPACK_DECODER: u64 = 0x03;

/// The role of one HTTP/3 unidirectional stream, classified by
/// its leading varint type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    /// `0x00` — control stream. One per direction. Carries
    /// connection-scoped frames (SETTINGS / GOAWAY / MAX_PUSH_ID
    /// / CANCEL_PUSH / etc.). Critical; closing it is a
    /// `H3_CLOSED_CRITICAL_STREAM` error.
    Control,
    /// `0x01` — push stream (server-initiated only). Carries a
    /// promised push response (HEADERS + DATA + trailers).
    Push,
    /// `0x02` — QPACK encoder stream (RFC 9204 §4.2). One per
    /// direction. Carries dynamic-table insert instructions.
    /// Critical.
    QpackEncoder,
    /// `0x03` — QPACK decoder stream (RFC 9204 §4.2). One per
    /// direction. Carries Section Acknowledgement / Stream
    /// Cancellation / Insert Count Increment. Critical.
    QpackDecoder,
    /// Any other type code. RFC 9114 §6.2.3 + §11.2.2 require
    /// forward-compat: unknown types are reserved-skip (close
    /// with `H3_STREAM_CREATION_ERROR` only when the slot is
    /// reserved for a unique-per-direction stream that already
    /// exists). v0.5's state machine applies the policy.
    Reserved(u64),
}

/// Read the leading varint from a uni-stream's body and
/// classify it. Returns the classified type + bytes consumed.
pub fn read_stream_type(buf: &[u8]) -> Result<(StreamType, usize), Error> {
    let mut cursor = buf;
    let raw = read_varint(&mut cursor)?;
    let consumed = buf.len() - cursor.len();
    Ok((classify(raw), consumed))
}

/// Write the type varint into `out` (appended). Returns the
/// byte count written.
pub fn write_stream_type(t: StreamType, out: &mut Vec<u8>) -> Result<usize, Error> {
    let raw = raw_value(t);
    let n = varint_len(raw);
    let start = out.len();
    let mut buf = [0u8; 8];
    write_varint(raw, &mut buf)?;
    out.extend_from_slice(&buf[..n]);
    Ok(out.len() - start)
}

/// `true` if closing this stream is a `H3_CLOSED_CRITICAL_STREAM`
/// connection error (RFC 9114 §6.2.1). Control + both QPACK
/// streams qualify; push streams do not (they can be
/// CANCEL_PUSH-ed without killing the connection).
pub fn is_critical(t: StreamType) -> bool {
    matches!(
        t,
        StreamType::Control | StreamType::QpackEncoder | StreamType::QpackDecoder
    )
}

/// `true` if more than one of this type per direction is a
/// `H3_STREAM_CREATION_ERROR` (RFC 9114 §6.2.1, §6.2.2; RFC
/// 9204 §4.2). Control + both QPACK streams qualify; push
/// streams + reserved types do not.
pub fn is_unique_per_direction(t: StreamType) -> bool {
    matches!(
        t,
        StreamType::Control | StreamType::QpackEncoder | StreamType::QpackDecoder
    )
}

// ============================================================
// Internals.
// ============================================================

fn classify(raw: u64) -> StreamType {
    match raw {
        TYPE_CONTROL => StreamType::Control,
        TYPE_PUSH => StreamType::Push,
        TYPE_QPACK_ENCODER => StreamType::QpackEncoder,
        TYPE_QPACK_DECODER => StreamType::QpackDecoder,
        other => StreamType::Reserved(other),
    }
}

fn raw_value(t: StreamType) -> u64 {
    match t {
        StreamType::Control => TYPE_CONTROL,
        StreamType::Push => TYPE_PUSH,
        StreamType::QpackEncoder => TYPE_QPACK_ENCODER,
        StreamType::QpackDecoder => TYPE_QPACK_DECODER,
        StreamType::Reserved(raw) => raw,
    }
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_types() {
        for t in [
            StreamType::Control,
            StreamType::Push,
            StreamType::QpackEncoder,
            StreamType::QpackDecoder,
        ] {
            let mut out = Vec::new();
            write_stream_type(t, &mut out).unwrap();
            let (got, n) = read_stream_type(&out).unwrap();
            assert_eq!(got, t);
            assert_eq!(n, out.len());
        }
    }

    #[test]
    fn classify_smoke() {
        assert_eq!(classify(0), StreamType::Control);
        assert_eq!(classify(0x03), StreamType::QpackDecoder);
        assert_eq!(classify(0x40), StreamType::Reserved(0x40));
        // RFC 9114 §11.2.2 greasing: 0x1f * N + 0x21 — first
        // grease value (N=0) is 0x21.
        assert_eq!(classify(0x21), StreamType::Reserved(0x21));
    }

    #[test]
    fn critical_smoke() {
        assert!(is_critical(StreamType::Control));
        assert!(is_critical(StreamType::QpackEncoder));
        assert!(is_critical(StreamType::QpackDecoder));
        assert!(!is_critical(StreamType::Push));
        assert!(!is_critical(StreamType::Reserved(0x21)));
    }
}
