//! HTTP/3 frame codec — RFC 9114 §7.2.
//!
//! Wire shape: `varint type | varint length | payload[length]`.
//! Unknown / reserved type codes are not errors — RFC 9114 §7.2.8
//! mandates forward-compat reserved-skip; we surface them as
//! [`Frame::Reserved`] so the per-stream dispatcher can decide
//! whether the slot the unknown frame appeared in is legal.
//!
//! No QPACK here — `Frame::Headers` / `Frame::PushPromise` carry
//! the QPACK-encoded byte string opaquely; v0.3's QPACK decoder
//! turns those bytes into a `Vec<Header>`.

use crate::Error;
use crate::varint::{read_varint, varint_len, write_varint};
use alloc::vec::Vec;

// ============================================================
// Frame type codes — RFC 9114 §7.2 + IANA registry.
// ============================================================

pub(crate) const TYPE_DATA: u64 = 0x00;
const TYPE_HEADERS: u64 = 0x01;
const TYPE_CANCEL_PUSH: u64 = 0x03;
const TYPE_SETTINGS: u64 = 0x04;
const TYPE_PUSH_PROMISE: u64 = 0x05;
const TYPE_GOAWAY: u64 = 0x07;
const TYPE_MAX_PUSH_ID: u64 = 0x0d;
const TYPE_ORIGIN: u64 = 0x0c;
const TYPE_PRIORITY_UPDATE_REQUEST: u64 = 0xf0700;
const TYPE_PRIORITY_UPDATE_PUSH: u64 = 0xf0701;

// ============================================================
// Public ADTs.
// ============================================================

/// One HTTP/3 frame. RFC 9114 §7.2 + the two PRIORITY_UPDATE
/// codes from RFC 9218.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// RFC 9114 §7.2.1 — DATA frame. Carries one chunk of a
    /// request / response body.
    Data {
        /// Body bytes.
        payload: Vec<u8>,
    },
    /// RFC 9114 §7.2.2 — HEADERS frame. The payload is a QPACK
    /// encoded header section; v0.3's QPACK decoder unpacks it.
    Headers {
        /// QPACK-encoded field section.
        encoded_field_section: Vec<u8>,
    },
    /// RFC 9114 §7.2.3 — CANCEL_PUSH frame. Sent by either side
    /// to abandon a server push.
    CancelPush {
        /// Push ID being cancelled.
        push_id: u64,
    },
    /// RFC 9114 §7.2.4 — SETTINGS frame. Connection-scoped
    /// parameter exchange. Payload is a sequence of `(id, value)`
    /// varint pairs; v0.5's connection state machine maps the
    /// IDs to fields on [`crate::Settings`].
    Settings(Vec<SettingPair>),
    /// RFC 9114 §7.2.5 — PUSH_PROMISE frame. Carries the
    /// push-promised request's header section + a push ID.
    PushPromise {
        /// Push ID for the promised stream.
        push_id: u64,
        /// QPACK-encoded request header section.
        encoded_field_section: Vec<u8>,
    },
    /// RFC 9114 §7.2.6 — GOAWAY frame. Carries the highest
    /// stream ID (server → client) or highest push ID
    /// (client → server) the sender has accepted; the peer must
    /// not initiate above this.
    Goaway {
        /// Highest accepted stream ID (server-sent) or push ID
        /// (client-sent).
        stream_or_push_id: u64,
    },
    /// RFC 9114 §7.2.7 — MAX_PUSH_ID frame. Client → server only.
    MaxPushId {
        /// Highest push ID the client will accept.
        push_id: u64,
    },
    /// RFC 9412 — ORIGIN frame. Carries one or more
    /// ASCII-Serialised-Origin entries, each `u16`-length-prefixed.
    Origin {
        /// One entry per origin advertised.
        entries: Vec<Vec<u8>>,
    },
    /// RFC 9218 §7.2 — PRIORITY_UPDATE frame. Two type codes
    /// (`0xf0700` for requests, `0xf0701` for pushes); collapsed
    /// here under `kind`.
    PriorityUpdate {
        /// Request-stream vs push-stream variant.
        kind: PriorityKind,
        /// Stream ID (kind = Request) or push ID (kind = Push)
        /// the priority applies to.
        prioritized_id: u64,
        /// The Priority Field Value (ASCII per RFC 9218; we
        /// hand it through opaquely).
        priority_field_value: Vec<u8>,
    },
    /// Forward-compat catch-all for frame types L5 doesn't
    /// recognise. RFC 9114 §7.2.8: unknown frames must be
    /// skipped; the per-stream dispatcher decides whether the
    /// slot is legal (e.g. SETTINGS only on the control stream).
    Reserved {
        /// Raw type varint value.
        type_: u64,
        /// Raw payload bytes.
        payload: Vec<u8>,
    },
}

/// PRIORITY_UPDATE flavour — request vs push (RFC 9218 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityKind {
    /// Request-stream PRIORITY_UPDATE (type `0xf0700`).
    Request,
    /// Push-stream PRIORITY_UPDATE (type `0xf0701`).
    Push,
}

/// One `(id, value)` pair in a SETTINGS frame payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingPair {
    /// Setting identifier (IANA-registered; e.g.
    /// `SETTINGS_MAX_FIELD_SECTION_SIZE` is `0x06`).
    pub id: u64,
    /// Setting value.
    pub value: u64,
}

// ============================================================
// Decode.
// ============================================================

/// Decode one frame from the front of `buf`, advancing `buf`
/// past the consumed bytes.
pub fn decode_frame(buf: &mut &[u8]) -> Result<Frame, Error> {
    let type_ = read_varint(buf)?;
    let length = read_varint(buf)? as usize;
    if buf.len() < length {
        return Err(Error::BadFrame);
    }
    let (payload, rest) = buf.split_at(length);
    *buf = rest;
    decode_body(type_, payload)
}

fn decode_body(type_: u64, mut payload: &[u8]) -> Result<Frame, Error> {
    match type_ {
        TYPE_DATA => Ok(Frame::Data {
            payload: payload.to_vec(),
        }),
        TYPE_HEADERS => Ok(Frame::Headers {
            encoded_field_section: payload.to_vec(),
        }),
        TYPE_CANCEL_PUSH => Ok(Frame::CancelPush {
            push_id: read_one_varint(&mut payload)?,
        }),
        TYPE_SETTINGS => decode_settings(payload),
        TYPE_PUSH_PROMISE => {
            let push_id = read_varint(&mut payload)?;
            Ok(Frame::PushPromise {
                push_id,
                encoded_field_section: payload.to_vec(),
            })
        }
        TYPE_GOAWAY => Ok(Frame::Goaway {
            stream_or_push_id: read_one_varint(&mut payload)?,
        }),
        TYPE_MAX_PUSH_ID => Ok(Frame::MaxPushId {
            push_id: read_one_varint(&mut payload)?,
        }),
        TYPE_ORIGIN => decode_origin(payload),
        TYPE_PRIORITY_UPDATE_REQUEST => decode_priority_update(PriorityKind::Request, payload),
        TYPE_PRIORITY_UPDATE_PUSH => decode_priority_update(PriorityKind::Push, payload),
        other => Ok(Frame::Reserved {
            type_: other,
            payload: payload.to_vec(),
        }),
    }
}

fn read_one_varint(buf: &mut &[u8]) -> Result<u64, Error> {
    let v = read_varint(buf)?;
    if !buf.is_empty() {
        return Err(Error::BadFrame);
    }
    Ok(v)
}

fn decode_settings(mut payload: &[u8]) -> Result<Frame, Error> {
    let mut pairs = Vec::new();
    while !payload.is_empty() {
        let id = read_varint(&mut payload)?;
        // RFC 9114 §7.2.4: a SETTINGS payload that ends mid-pair
        // is malformed; the value varint must be present.
        if payload.is_empty() {
            return Err(Error::BadFrame);
        }
        let value = read_varint(&mut payload)?;
        pairs.push(SettingPair { id, value });
    }
    Ok(Frame::Settings(pairs))
}

fn decode_origin(mut payload: &[u8]) -> Result<Frame, Error> {
    let mut entries = Vec::new();
    while !payload.is_empty() {
        if payload.len() < 2 {
            return Err(Error::BadFrame);
        }
        let len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
        if payload.len() < 2 + len {
            return Err(Error::BadFrame);
        }
        entries.push(payload[2..2 + len].to_vec());
        payload = &payload[2 + len..];
    }
    Ok(Frame::Origin { entries })
}

fn decode_priority_update(kind: PriorityKind, mut payload: &[u8]) -> Result<Frame, Error> {
    let prioritized_id = read_varint(&mut payload)?;
    Ok(Frame::PriorityUpdate {
        kind,
        prioritized_id,
        priority_field_value: payload.to_vec(),
    })
}

// ============================================================
// Encode.
// ============================================================

/// Encode one frame into `out` (appended), returning the byte
/// count written.
pub fn encode_frame(frame: &Frame, out: &mut Vec<u8>) -> Result<usize, Error> {
    let start = out.len();
    match frame {
        Frame::Data { payload } => emit(out, TYPE_DATA, payload)?,
        Frame::Headers {
            encoded_field_section,
        } => emit(out, TYPE_HEADERS, encoded_field_section)?,
        Frame::CancelPush { push_id } => emit_varint_only(out, TYPE_CANCEL_PUSH, *push_id)?,
        Frame::Settings(pairs) => {
            let mut body = Vec::new();
            for pair in pairs {
                put_varint(&mut body, pair.id)?;
                put_varint(&mut body, pair.value)?;
            }
            emit(out, TYPE_SETTINGS, &body)?
        }
        Frame::PushPromise {
            push_id,
            encoded_field_section,
        } => {
            let mut body = Vec::with_capacity(8 + encoded_field_section.len());
            put_varint(&mut body, *push_id)?;
            body.extend_from_slice(encoded_field_section);
            emit(out, TYPE_PUSH_PROMISE, &body)?
        }
        Frame::Goaway { stream_or_push_id } => {
            emit_varint_only(out, TYPE_GOAWAY, *stream_or_push_id)?
        }
        Frame::MaxPushId { push_id } => emit_varint_only(out, TYPE_MAX_PUSH_ID, *push_id)?,
        Frame::Origin { entries } => {
            let mut body = Vec::new();
            for entry in entries {
                if entry.len() > u16::MAX as usize {
                    return Err(Error::BadFrame);
                }
                let len = entry.len() as u16;
                body.extend_from_slice(&len.to_be_bytes());
                body.extend_from_slice(entry);
            }
            emit(out, TYPE_ORIGIN, &body)?
        }
        Frame::PriorityUpdate {
            kind,
            prioritized_id,
            priority_field_value,
        } => {
            let type_ = match kind {
                PriorityKind::Request => TYPE_PRIORITY_UPDATE_REQUEST,
                PriorityKind::Push => TYPE_PRIORITY_UPDATE_PUSH,
            };
            let mut body = Vec::with_capacity(8 + priority_field_value.len());
            put_varint(&mut body, *prioritized_id)?;
            body.extend_from_slice(priority_field_value);
            emit(out, type_, &body)?
        }
        Frame::Reserved { type_, payload } => emit(out, *type_, payload)?,
    }
    Ok(out.len() - start)
}

fn emit(out: &mut Vec<u8>, type_: u64, payload: &[u8]) -> Result<(), Error> {
    put_varint(out, type_)?;
    put_varint(out, payload.len() as u64)?;
    out.extend_from_slice(payload);
    Ok(())
}

fn emit_varint_only(out: &mut Vec<u8>, type_: u64, value: u64) -> Result<(), Error> {
    let body_len = varint_len(value);
    put_varint(out, type_)?;
    put_varint(out, body_len as u64)?;
    put_varint(out, value)?;
    Ok(())
}

fn put_varint(out: &mut Vec<u8>, value: u64) -> Result<(), Error> {
    let mut buf = [0u8; 8];
    let n = write_varint(value, &mut buf)?;
    out.extend_from_slice(&buf[..n]);
    Ok(())
}

/// Return the on-wire length the shortest varint encoding of
/// `value` occupies. Convenient for stream-budget sizing.
pub fn frame_varint_len(value: u64) -> usize {
    varint_len(value)
}
