//! Wire-format v2 single-pass header codec.
//!
//! `decode_header` is the hot-path entry: it reads
//! sixteen bytes from a fixed-size array, performs no
//! allocation, and branches only on the `frame_type`
//! enum match (LLVM lowers it to a jump table at -O ≥ 2).
//!
//! `encode_header` is the symmetric writer: builds the
//! 16-byte header into a fixed-size buffer.

use super::errors::WireError;
use super::frame::{Flags, FrameType, HEADER_LEN, Header};

/// Decode a fixed-size 16-byte header buffer (SPEC § 2).
///
/// The `reserved` field (bytes 12..16) is ignored per
/// SPEC § 2.1's forward-compatibility clause.
#[inline]
pub fn decode_header(bytes: &[u8; HEADER_LEN]) -> Result<Header, WireError> {
    let stream_id = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let frame_type = FrameType::from_u8(bytes[4])?;
    let flags = Flags(bytes[5]);
    let metadata_count = u16::from_le_bytes([bytes[6], bytes[7]]);
    let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    Ok(Header {
        stream_id,
        frame_type,
        flags,
        metadata_count,
        payload_len,
    })
}

/// Decode a header from a possibly-larger slice. Returns
/// `WireError::BufferTooShort` if the slice is under 16
/// bytes.
#[inline]
pub fn decode_header_from_slice(bytes: &[u8]) -> Result<Header, WireError> {
    if bytes.len() < HEADER_LEN {
        return Err(WireError::BufferTooShort {
            need: HEADER_LEN,
            have: bytes.len(),
        });
    }
    let mut buf = [0u8; HEADER_LEN];
    buf.copy_from_slice(&bytes[..HEADER_LEN]);
    decode_header(&buf)
}

/// Encode a header into a fixed-size 16-byte buffer.
/// `reserved` is written as zero per SPEC § 2.1.
#[inline]
pub fn encode_header(h: &Header, out: &mut [u8; HEADER_LEN]) {
    out[0..4].copy_from_slice(&h.stream_id.to_le_bytes());
    out[4] = h.frame_type.to_u8();
    out[5] = h.flags.bits();
    out[6..8].copy_from_slice(&h.metadata_count.to_le_bytes());
    out[8..12].copy_from_slice(&h.payload_len.to_le_bytes());
    out[12..16].fill(0);
}
