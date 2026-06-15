//! QUIC packet header codec — RFC 9000 §17.
//!
//! v0.2 lands the **unprotected** wire form (encode + decode for
//! Long header Initial / Handshake / 0-RTT and Short header 1-RTT).
//! AEAD packet protection + header protection move to v0.3.
//!
//! The decode side returns borrowed slices for DCID / SCID / token
//! (the inputs are caller-owned buffers); the owned [`ConnectionId`]
//! wrapper is only constructed when the caller wants to retain a
//! connection-level handle.

use crate::varint::{read_varint, varint_len, write_varint};
use crate::{ConnectionId, Error};
use alloc::vec::Vec;

/// QUIC v1 long-header packet type, RFC 9000 §17.2. The 2-bit type
/// field maps to the four long-header packet kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongHeaderType {
    /// Initial packet (RFC 9000 §17.2.2).
    Initial,
    /// 0-RTT packet (RFC 9000 §17.2.3).
    ZeroRtt,
    /// Handshake packet (RFC 9000 §17.2.4).
    Handshake,
    /// Retry packet (RFC 9000 §17.2.5). Body framing deferred to
    /// v0.4 (Retry integrity tag needs AEAD).
    Retry,
}

impl LongHeaderType {
    fn from_bits(bits: u8) -> Result<Self, Error> {
        // RFC 9000 §17.2 type field bits 0-1 of the long-header
        // first byte's lower nibble. The header masks `bits` from
        // the first byte; we receive the 2-bit value directly.
        match bits {
            0b00 => Ok(LongHeaderType::Initial),
            0b01 => Ok(LongHeaderType::ZeroRtt),
            0b10 => Ok(LongHeaderType::Handshake),
            0b11 => Ok(LongHeaderType::Retry),
            _ => Err(Error::BadPacket),
        }
    }

    fn to_bits(self) -> u8 {
        match self {
            LongHeaderType::Initial => 0b00,
            LongHeaderType::ZeroRtt => 0b01,
            LongHeaderType::Handshake => 0b10,
            LongHeaderType::Retry => 0b11,
        }
    }
}

/// QUIC v1 long-header packet (Initial / 0-RTT / Handshake).
///
/// `payload_len` is the length field that prefixes the
/// packet-number + payload section per RFC 9000 §17.2 — encoded as
/// a varint on the wire. `packet_number` is the **truncated**
/// `pn_len`-byte form; the full-PN reconstruction (RFC 9000 §17.1)
/// happens in v0.5 loss recovery, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongHeader {
    /// Packet type (Initial / 0-RTT / Handshake / Retry).
    pub type_: LongHeaderType,
    /// QUIC version. v0.2 only supports v1 (`0x00000001`).
    pub version: u32,
    /// Destination Connection ID.
    pub dcid: ConnectionId,
    /// Source Connection ID.
    pub scid: ConnectionId,
    /// Token (Initial packets only; `None` for other types).
    pub token: Option<Vec<u8>>,
    /// Length of the packet-number + protected-payload section.
    pub payload_len: u64,
    /// Truncated packet number (the `pn_len + 1` low bytes on the
    /// wire).
    pub packet_number: u32,
    /// Wire packet-number length (1-4 bytes). Encoded as `pn_len - 1`
    /// in the header's first byte's low 2 bits.
    pub pn_len: u8,
}

/// QUIC v1 short-header packet (1-RTT).
///
/// `dcid` length is **not** carried on the wire (RFC 9000 §17.3.1
/// — the receiver knows the negotiated DCID length per connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortHeader {
    /// Destination Connection ID.
    pub dcid: ConnectionId,
    /// Truncated packet number.
    pub packet_number: u32,
    /// Wire packet-number length (1-4 bytes).
    pub pn_len: u8,
    /// Key Phase bit (toggles on key update; RFC 9001 §6).
    pub key_phase: bool,
    /// Latency Spin bit (RFC 9000 §17.4 / RFC 9312).
    pub spin: bool,
}

/// A QUIC packet header — Long or Short form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketHeader {
    /// Long-header packet (Initial / 0-RTT / Handshake / Retry).
    Long(LongHeader),
    /// Short-header packet (1-RTT).
    Short(ShortHeader),
}

/// Encode `header` into `out`. Appends the bytes (including the
/// length-field for long headers) but does **not** include the
/// protected payload — the caller writes that separately after.
/// Returns the number of bytes appended.
pub fn encode_header(header: &PacketHeader, out: &mut Vec<u8>) -> Result<usize, Error> {
    let start = out.len();
    match header {
        PacketHeader::Long(h) => {
            encode_long_header(h, out)?;
        }
        PacketHeader::Short(h) => {
            encode_short_header(h, out)?;
        }
    }
    Ok(out.len() - start)
}

fn encode_long_header(h: &LongHeader, out: &mut Vec<u8>) -> Result<usize, Error> {
    if h.dcid.len > 20 || h.scid.len > 20 {
        return Err(Error::BadPacket);
    }
    if !(1..=4).contains(&h.pn_len) {
        return Err(Error::BadPacket);
    }
    let start = out.len();

    // First byte: 1100 type<<4 | pn_len-1
    let first = 0b1100_0000 | (h.type_.to_bits() << 4) | (h.pn_len - 1);
    out.push(first);

    // Version (4 bytes big-endian).
    out.extend_from_slice(&h.version.to_be_bytes());

    // DCID Len + DCID.
    out.push(h.dcid.len);
    out.extend_from_slice(&h.dcid.bytes[..h.dcid.len as usize]);

    // SCID Len + SCID.
    out.push(h.scid.len);
    out.extend_from_slice(&h.scid.bytes[..h.scid.len as usize]);

    // Initial-only: Token Length varint + Token.
    if h.type_ == LongHeaderType::Initial {
        let token = h.token.as_deref().unwrap_or(&[]);
        let mut buf = [0u8; 8];
        let n = write_varint(token.len() as u64, &mut buf)?;
        out.extend_from_slice(&buf[..n]);
        out.extend_from_slice(token);
    }

    // Length varint (covers PN + payload).
    {
        let mut buf = [0u8; 8];
        let n = write_varint(h.payload_len, &mut buf)?;
        out.extend_from_slice(&buf[..n]);
    }

    // Packet number (pn_len bytes, big-endian truncated).
    let pn_bytes = h.packet_number.to_be_bytes();
    out.extend_from_slice(&pn_bytes[4 - h.pn_len as usize..]);

    Ok(out.len() - start)
}

fn encode_short_header(h: &ShortHeader, out: &mut Vec<u8>) -> Result<usize, Error> {
    if h.dcid.len > 20 {
        return Err(Error::BadPacket);
    }
    if !(1..=4).contains(&h.pn_len) {
        return Err(Error::BadPacket);
    }
    let start = out.len();

    // First byte: 0|1 (header form) | 1 (fixed bit) | spin | 0 0
    // (reserved) | key_phase | pn_len-1
    let first =
        0b0100_0000 | (u8::from(h.spin) << 5) | (u8::from(h.key_phase) << 2) | (h.pn_len - 1);
    out.push(first);

    // DCID (no length prefix on the wire).
    out.extend_from_slice(&h.dcid.bytes[..h.dcid.len as usize]);

    // Packet number (pn_len bytes, big-endian truncated).
    let pn_bytes = h.packet_number.to_be_bytes();
    out.extend_from_slice(&pn_bytes[4 - h.pn_len as usize..]);

    Ok(out.len() - start)
}

/// Decode a packet header from the front of `buf`. For a Short
/// header (1-RTT) the caller must supply `dcid_len` (the
/// negotiated DCID length, known per-connection). Returns the
/// decoded header + the number of bytes consumed.
pub fn decode_header(buf: &[u8], dcid_len: usize) -> Result<(PacketHeader, usize), Error> {
    if buf.is_empty() {
        return Err(Error::BadPacket);
    }
    if buf[0] & 0x80 == 0 {
        decode_short_header(buf, dcid_len)
    } else {
        decode_long_header(buf)
    }
}

fn decode_long_header(buf: &[u8]) -> Result<(PacketHeader, usize), Error> {
    let mut cursor: &[u8] = buf;
    let first = read_u8(&mut cursor)?;
    // Fixed bit (RFC 9000 §17.2): MUST be 1.
    if first & 0x40 == 0 {
        return Err(Error::BadPacket);
    }
    let pn_len = (first & 0x03) + 1;
    let type_bits = (first >> 4) & 0x03;
    let type_ = LongHeaderType::from_bits(type_bits)?;

    let version = u32::from_be_bytes(read_array::<4>(&mut cursor)?);

    let dcid_len = read_u8(&mut cursor)? as usize;
    if dcid_len > 20 {
        return Err(Error::BadPacket);
    }
    let dcid_bytes = read_slice(&mut cursor, dcid_len)?;
    let dcid = make_cid(dcid_bytes);

    let scid_len = read_u8(&mut cursor)? as usize;
    if scid_len > 20 {
        return Err(Error::BadPacket);
    }
    let scid_bytes = read_slice(&mut cursor, scid_len)?;
    let scid = make_cid(scid_bytes);

    let token = if type_ == LongHeaderType::Initial {
        let token_len = read_varint(&mut cursor)? as usize;
        let token_bytes = read_slice(&mut cursor, token_len)?;
        Some(token_bytes.to_vec())
    } else {
        None
    };

    let payload_len = read_varint(&mut cursor)?;

    let pn_bytes = read_slice(&mut cursor, pn_len as usize)?;
    let mut pn_pad = [0u8; 4];
    pn_pad[4 - pn_len as usize..].copy_from_slice(pn_bytes);
    let packet_number = u32::from_be_bytes(pn_pad);

    let consumed = buf.len() - cursor.len();
    Ok((
        PacketHeader::Long(LongHeader {
            type_,
            version,
            dcid,
            scid,
            token,
            payload_len,
            packet_number,
            pn_len,
        }),
        consumed,
    ))
}

fn decode_short_header(buf: &[u8], dcid_len: usize) -> Result<(PacketHeader, usize), Error> {
    if dcid_len > 20 {
        return Err(Error::BadPacket);
    }
    let mut cursor: &[u8] = buf;
    let first = read_u8(&mut cursor)?;
    // Fixed bit MUST be 1.
    if first & 0x40 == 0 {
        return Err(Error::BadPacket);
    }
    let pn_len = (first & 0x03) + 1;
    let key_phase = first & 0x04 != 0;
    let spin = first & 0x20 != 0;

    let dcid_bytes = read_slice(&mut cursor, dcid_len)?;
    let dcid = make_cid(dcid_bytes);

    let pn_bytes = read_slice(&mut cursor, pn_len as usize)?;
    let mut pn_pad = [0u8; 4];
    pn_pad[4 - pn_len as usize..].copy_from_slice(pn_bytes);
    let packet_number = u32::from_be_bytes(pn_pad);

    let consumed = buf.len() - cursor.len();
    Ok((
        PacketHeader::Short(ShortHeader {
            dcid,
            packet_number,
            pn_len,
            key_phase,
            spin,
        }),
        consumed,
    ))
}

// ============================================================
// Helpers.
// ============================================================

fn make_cid(bytes: &[u8]) -> ConnectionId {
    let mut cid = ConnectionId {
        bytes: [0u8; 20],
        len: bytes.len() as u8,
    };
    cid.bytes[..bytes.len()].copy_from_slice(bytes);
    cid
}

fn read_u8(cursor: &mut &[u8]) -> Result<u8, Error> {
    let &[b, ref rest @ ..] = *cursor else {
        return Err(Error::BadPacket);
    };
    *cursor = rest;
    Ok(b)
}

fn read_array<const N: usize>(cursor: &mut &[u8]) -> Result<[u8; N], Error> {
    if cursor.len() < N {
        return Err(Error::BadPacket);
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&cursor[..N]);
    *cursor = &cursor[N..];
    Ok(out)
}

fn read_slice<'a>(cursor: &mut &'a [u8], n: usize) -> Result<&'a [u8], Error> {
    if cursor.len() < n {
        return Err(Error::BadPacket);
    }
    let (head, tail) = cursor.split_at(n);
    *cursor = tail;
    Ok(head)
}

/// Return the on-wire length the shortest encoding of `len` will
/// occupy as a varint length prefix. Helpful for sizing the
/// `payload_len` field before encoding.
pub fn varint_field_len(len: u64) -> usize {
    varint_len(len)
}
