//! HTTP/2 frame codec — RFC 9113 §4.
//!
//! 9-byte header: length(24) | type(8) | flags(8) |
//! R+stream-id(31). The subset sufficient for gRPC
//! interop: DATA / HEADERS / RST_STREAM / SETTINGS /
//! PING / GOAWAY / WINDOW_UPDATE / CONTINUATION.
//! PRIORITY parses-and-skips (deprecated by RFC 9113);
//! PUSH_PROMISE is rejected (we advertise
//! `ENABLE_PUSH = 0`).

use alloc::vec::Vec;

/// H2 failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H2Error {
    /// Input ended mid-frame (streaming condition at the
    /// connection layer; hard error at the codec layer
    /// only for declared-length overruns).
    Truncated,
    /// Malformed frame (bad padding, reserved violations).
    BadFrame,
    /// HPACK failure inside a header block.
    Hpack(crate::hpack::HpackError),
    /// Peer protocol violation (push without permission,
    /// unknown required behaviour, bad preface).
    Protocol(&'static str),
    /// Flow-control accounting violation.
    FlowControl,
    /// Link liveness (0.2.0): no inbound traffic past the
    /// idle timeout — the link is dead.
    LinkDead,
}

impl From<crate::hpack::HpackError> for H2Error {
    fn from(e: crate::hpack::HpackError) -> Self {
        H2Error::Hpack(e)
    }
}

/// Frame types (RFC 9113 §6).
pub const TYPE_DATA: u8 = 0x0;
/// HEADERS frame type.
pub const TYPE_HEADERS: u8 = 0x1;
/// PRIORITY frame type (parse-and-skip).
pub const TYPE_PRIORITY: u8 = 0x2;
/// RST_STREAM frame type.
pub const TYPE_RST_STREAM: u8 = 0x3;
/// SETTINGS frame type.
pub const TYPE_SETTINGS: u8 = 0x4;
/// PUSH_PROMISE frame type (rejected).
pub const TYPE_PUSH_PROMISE: u8 = 0x5;
/// PING frame type.
pub const TYPE_PING: u8 = 0x6;
/// GOAWAY frame type.
pub const TYPE_GOAWAY: u8 = 0x7;
/// WINDOW_UPDATE frame type.
pub const TYPE_WINDOW_UPDATE: u8 = 0x8;
/// CONTINUATION frame type.
pub const TYPE_CONTINUATION: u8 = 0x9;

/// `END_STREAM` flag (DATA / HEADERS).
pub const FLAG_END_STREAM: u8 = 0x1;
/// `ACK` flag (SETTINGS / PING).
pub const FLAG_ACK: u8 = 0x1;
/// `END_HEADERS` flag (HEADERS / CONTINUATION).
pub const FLAG_END_HEADERS: u8 = 0x4;
/// `PADDED` flag (DATA / HEADERS).
pub const FLAG_PADDED: u8 = 0x8;
/// `PRIORITY` flag (HEADERS).
pub const FLAG_PRIORITY: u8 = 0x20;

/// One raw H2 frame (header parsed, payload as on the
/// wire — padding/priority stripping happens in
/// [`payload_body`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Frame type code.
    pub ty: u8,
    /// Flag bits.
    pub flags: u8,
    /// Stream id (R bit cleared).
    pub stream_id: u32,
    /// Raw payload (length-delimited by the header).
    pub payload: Vec<u8>,
}

/// Encode a frame onto `out`.
///
/// # Panics
/// If `payload.len()` exceeds 2^24-1 (the caller chunks
/// to the negotiated max frame size, default 16384, far
/// below).
pub fn encode_frame(ty: u8, flags: u8, stream_id: u32, payload: &[u8], out: &mut Vec<u8>) {
    let len = payload.len();
    assert!(len < (1 << 24), "h2 frame payload exceeds 24-bit length");
    out.push((len >> 16) as u8);
    out.push((len >> 8) as u8);
    out.push(len as u8);
    out.push(ty);
    out.push(flags);
    out.extend_from_slice(&(stream_id & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Try to decode the leading frame. `Ok(None)` = more
/// bytes needed; `Ok(Some((frame, consumed)))` otherwise.
/// `max_frame_size` guards against absurd declared
/// lengths (peer violation → error, not pending).
pub fn try_decode_frame(
    buf: &[u8],
    max_frame_size: usize,
) -> Result<Option<(RawFrame, usize)>, H2Error> {
    if buf.len() < 9 {
        return Ok(None);
    }
    let len = ((buf[0] as usize) << 16) | ((buf[1] as usize) << 8) | buf[2] as usize;
    if len > max_frame_size {
        return Err(H2Error::Protocol("frame exceeds max frame size"));
    }
    if buf.len() < 9 + len {
        return Ok(None);
    }
    let ty = buf[3];
    let flags = buf[4];
    let stream_id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
    Ok(Some((
        RawFrame {
            ty,
            flags,
            stream_id,
            payload: buf[9..9 + len].to_vec(),
        },
        9 + len,
    )))
}

/// Strip PADDED / PRIORITY decoration from a DATA or
/// HEADERS payload, returning the body slice.
pub fn payload_body(frame_ty: u8, flags: u8, payload: &[u8]) -> Result<&[u8], H2Error> {
    let mut body = payload;
    let mut pad_len = 0usize;
    if flags & FLAG_PADDED != 0 {
        let (&p, rest) = body.split_first().ok_or(H2Error::BadFrame)?;
        pad_len = p as usize;
        body = rest;
    }
    if frame_ty == TYPE_HEADERS && flags & FLAG_PRIORITY != 0 {
        // 4-byte stream dependency + 1-byte weight.
        if body.len() < 5 {
            return Err(H2Error::BadFrame);
        }
        body = &body[5..];
    }
    if pad_len > body.len() {
        return Err(H2Error::BadFrame);
    }
    Ok(&body[..body.len() - pad_len])
}

// ============================================================
// SETTINGS payload (RFC 9113 §6.5: 6-byte id/value pairs).
// ============================================================

/// SETTINGS_HEADER_TABLE_SIZE.
pub const SETTINGS_HEADER_TABLE_SIZE: u16 = 0x1;
/// SETTINGS_ENABLE_PUSH.
pub const SETTINGS_ENABLE_PUSH: u16 = 0x2;
/// SETTINGS_INITIAL_WINDOW_SIZE.
pub const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
/// SETTINGS_MAX_FRAME_SIZE.
pub const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;

/// Parse a SETTINGS payload into `(id, value)` pairs.
pub fn parse_settings(payload: &[u8]) -> Result<Vec<(u16, u32)>, H2Error> {
    if !payload.len().is_multiple_of(6) {
        return Err(H2Error::BadFrame);
    }
    Ok(payload
        .chunks_exact(6)
        .map(|c| {
            (
                u16::from_be_bytes([c[0], c[1]]),
                u32::from_be_bytes([c[2], c[3], c[4], c[5]]),
            )
        })
        .collect())
}

/// Serialize `(id, value)` pairs into a SETTINGS payload.
pub fn encode_settings(pairs: &[(u16, u32)], out: &mut Vec<u8>) {
    for &(id, value) in pairs {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&value.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn frame_round_trip_and_incremental() {
        let mut wire = Vec::new();
        encode_frame(TYPE_DATA, FLAG_END_STREAM, 5, b"payload", &mut wire);
        for cut in 0..wire.len() {
            assert_eq!(try_decode_frame(&wire[..cut], 16384).unwrap(), None);
        }
        let (f, n) = try_decode_frame(&wire, 16384).unwrap().unwrap();
        assert_eq!(n, wire.len());
        assert_eq!(f.ty, TYPE_DATA);
        assert_eq!(f.flags, FLAG_END_STREAM);
        assert_eq!(f.stream_id, 5);
        assert_eq!(f.payload, b"payload");
    }

    #[test]
    fn oversized_declared_length_is_protocol_error() {
        // 1 MiB declared with a 16 KiB cap.
        let hdr = [0x10, 0x00, 0x00, TYPE_DATA, 0, 0, 0, 0, 1];
        assert!(matches!(
            try_decode_frame(&hdr, 16384),
            Err(H2Error::Protocol(_))
        ));
    }

    #[test]
    fn padded_and_priority_stripping() {
        // HEADERS with PADDED+PRIORITY: padlen=2, 5B
        // priority, body "hi", 2B padding.
        let payload = [2u8, 0, 0, 0, 3, 15, b'h', b'i', 0, 0];
        let body = payload_body(TYPE_HEADERS, FLAG_PADDED | FLAG_PRIORITY, &payload).unwrap();
        assert_eq!(body, b"hi");
        // Padding longer than body rejected.
        assert!(payload_body(TYPE_DATA, FLAG_PADDED, &[9, 1, 2]).is_err());
    }

    #[test]
    fn settings_round_trip() {
        let pairs = vec![(SETTINGS_ENABLE_PUSH, 0), (SETTINGS_MAX_FRAME_SIZE, 16384)];
        let mut p = Vec::new();
        encode_settings(&pairs, &mut p);
        assert_eq!(parse_settings(&p).unwrap(), pairs);
        assert!(parse_settings(&[0; 5]).is_err());
    }
}
