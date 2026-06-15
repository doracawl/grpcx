// gRPC message framing — SPEC.md § "gRPC message framing".
//
// Every gRPC message (request or response, streaming chunk
// or final) is framed as:
//
//   1 byte  compressed-flag (0 = identity, 1 = compressed)
//   4 bytes payload length, big-endian
//   N bytes payload
//
// This module is the pure byte codec: it neither inspects
// payloads nor enforces per-call policy (max message size,
// compression negotiation) — those are call-layer concerns
// (v0.3+). Symmetric encode/decode: every frame the decoder
// accepts, the encoder produces, and vice versa.

use crate::RpcError;
use alloc::vec::Vec;

/// One length-prefixed gRPC message frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MessageFrame {
    /// Compressed-flag from the wire. `false` = identity
    /// (the only mode v0.2 produces); `true` = compressed
    /// with the algorithm named by `grpc-encoding`. The
    /// codec transports the bit faithfully — rejecting
    /// un-negotiated compression is the call layer's job.
    pub compressed: bool,
    /// The message payload (serialized protobuf bytes).
    pub payload: Vec<u8>,
}

/// Size of the fixed frame header (flag byte + 4-byte
/// big-endian length).
pub const FRAME_HEADER_LEN: usize = 5;

/// Encode one message frame onto `out`.
///
/// Appends exactly `FRAME_HEADER_LEN + payload.len()`
/// bytes.
///
/// # Panics
/// If `payload.len()` exceeds `u32::MAX` (the length field
/// is 4 bytes; gRPC cannot carry larger messages in one
/// frame).
pub fn encode_frame(frame: &MessageFrame, out: &mut Vec<u8>) {
    let len = u32::try_from(frame.payload.len())
        .expect("gRPC frame payload exceeds u32::MAX (4 GiB) — unrepresentable on the wire");
    out.reserve(FRAME_HEADER_LEN + frame.payload.len());
    out.push(u8::from(frame.compressed));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&frame.payload);
}

/// Decode the leading message frame from `bytes`.
///
/// Returns `(frame, bytes_consumed)`.
///
/// # Errors
/// - [`RpcError::Codec`] — input shorter than the frame
///   header + declared payload length ("truncated gRPC
///   frame"), or the compressed-flag byte is neither 0
///   nor 1 ("invalid gRPC compressed-flag").
pub fn decode_frame(bytes: &[u8]) -> Result<(MessageFrame, usize), RpcError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Err(RpcError::Codec("truncated gRPC frame header"));
    }
    let compressed = match bytes[0] {
        0 => false,
        1 => true,
        _ => return Err(RpcError::Codec("invalid gRPC compressed-flag")),
    };
    let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    let total = FRAME_HEADER_LEN + len;
    if bytes.len() < total {
        return Err(RpcError::Codec("truncated gRPC frame payload"));
    }
    Ok((
        MessageFrame {
            compressed,
            payload: bytes[FRAME_HEADER_LEN..total].to_vec(),
        },
        total,
    ))
}

/// Decode every frame in `bytes` (the full body of a
/// stream). Errors if any frame is malformed or if
/// trailing bytes remain after the last complete frame.
///
/// # Errors
/// Same modes as [`decode_frame`], applied per frame.
pub fn decode_all_frames(mut bytes: &[u8]) -> Result<Vec<MessageFrame>, RpcError> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        let (frame, consumed) = decode_frame(bytes)?;
        frames.push(frame);
        bytes = &bytes[consumed..];
    }
    Ok(frames)
}

/// Incremental decode for streaming bodies: `Ok(None)`
/// when `bytes` holds only a *prefix* of a frame (more
/// data must arrive), `Ok(Some((frame, consumed)))` for a
/// complete leading frame.
///
/// # Errors
/// [`RpcError::Codec`] only for *structurally* invalid
/// input (compressed-flag ∉ {0, 1}) — truncation is the
/// expected streaming condition, not an error.
pub fn try_decode_frame(bytes: &[u8]) -> Result<Option<(MessageFrame, usize)>, RpcError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Ok(None);
    }
    if bytes[0] > 1 {
        return Err(RpcError::Codec("invalid gRPC compressed-flag"));
    }
    let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    if bytes.len() < FRAME_HEADER_LEN + len {
        return Ok(None);
    }
    decode_frame(bytes).map(Some)
}
