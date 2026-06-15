//! QUIC frame codec — RFC 9000 §19 + RFC 9221 (DATAGRAM).
//!
//! v0.4 ships an owned [`Frame`] enum (`Vec<u8>` for byte
//! payloads). The borrow-only `FrameRef<'a>` (design intent #1
//! zero-alloc hot path) is a v0.x+ perf-pass extension once the
//! connection state machine in v0.5 surfaces the actual hot
//! path.

use crate::varint::{read_varint, varint_len, write_varint};
use crate::{Error, StreamId};
use alloc::vec::Vec;

/// One QUIC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// RFC 9000 §19.1 — padding (one byte per frame; encoders
    /// typically emit runs of padding by writing N `Padding`
    /// frames or by allocating a sized buffer pre-encode).
    Padding,
    /// RFC 9000 §19.2 — keep-alive ping.
    Ping,
    /// RFC 9000 §19.3 — ACK frame. `ranges` is the list of
    /// (gap, range) pairs **after** the first range; the first
    /// range is `first_ack_range`. Carries optional ECN counts
    /// when present.
    Ack {
        /// Largest packet number being acknowledged.
        largest_acked: u64,
        /// ACK delay (in microseconds, scaled by the receiver's
        /// `ack_delay_exponent`; v0.4 carries the raw varint).
        ack_delay: u64,
        /// First ack range (number of contiguous packets below
        /// `largest_acked` also being acknowledged).
        first_ack_range: u64,
        /// (gap, range) pairs descending in PN. Empty if the ACK
        /// covers only the first range.
        ranges: Vec<(u64, u64)>,
        /// Optional ECN counts (ECT0, ECT1, CE). Present iff the
        /// frame's type byte is `0x03`.
        ecn: Option<EcnCounts>,
    },
    /// RFC 9000 §19.4.
    ResetStream {
        /// Stream the sender is resetting.
        stream_id: StreamId,
        /// Application-protocol error code.
        application_error_code: u64,
        /// Final size delivered on the stream.
        final_size: u64,
    },
    /// RFC 9000 §19.5.
    StopSending {
        /// Stream the receiver wants the peer to stop sending on.
        stream_id: StreamId,
        /// Application-protocol error code.
        application_error_code: u64,
    },
    /// RFC 9000 §19.6.
    Crypto {
        /// Offset within the CRYPTO stream.
        offset: u64,
        /// Crypto handshake bytes (CRYPTO frames are
        /// length-prefixed but carry no FIN — handshakes don't
        /// "end" per stream).
        data: Vec<u8>,
    },
    /// RFC 9000 §19.7 — server-only NEW_TOKEN.
    NewToken {
        /// Opaque token bytes.
        token: Vec<u8>,
    },
    /// RFC 9000 §19.8 — STREAM frame; covers all 8 wire variants
    /// `(FIN, LEN, OFF)` via the `fin` flag + presence of
    /// `offset` (offset=0 means no OFF bit).
    Stream {
        /// Stream id (parsed varint).
        stream_id: StreamId,
        /// Byte offset within the stream (0 for the first chunk).
        offset: u64,
        /// Payload bytes.
        data: Vec<u8>,
        /// Carries a FIN.
        fin: bool,
    },
    /// RFC 9000 §19.9 — MAX_DATA (connection-level credit).
    MaxData(u64),
    /// RFC 9000 §19.10 — per-stream credit.
    MaxStreamData {
        /// Stream id.
        stream_id: StreamId,
        /// Maximum data the sender is willing to accept on the
        /// stream.
        max_data: u64,
    },
    /// RFC 9000 §19.11 — MAX_STREAMS for bidirectional or
    /// unidirectional streams.
    MaxStreams {
        /// `true` = bidirectional, `false` = unidirectional.
        bidi: bool,
        /// Maximum stream count.
        max: u64,
    },
    /// RFC 9000 §19.12.
    DataBlocked {
        /// Connection-level data limit the sender is blocked at.
        data_limit: u64,
    },
    /// RFC 9000 §19.13.
    StreamDataBlocked {
        /// Stream id.
        stream_id: StreamId,
        /// Per-stream data limit the sender is blocked at.
        stream_data_limit: u64,
    },
    /// RFC 9000 §19.14.
    StreamsBlocked {
        /// `true` = bidirectional, `false` = unidirectional.
        bidi: bool,
        /// Stream limit the sender is blocked at.
        stream_limit: u64,
    },
    /// RFC 9000 §19.15.
    NewConnectionId {
        /// Sequence number for the issued CID.
        sequence_number: u64,
        /// Retire-prior-to bound.
        retire_prior_to: u64,
        /// Connection ID bytes (1-20 bytes per RFC 9000 §17.2).
        connection_id: Vec<u8>,
        /// 128-bit stateless reset token.
        stateless_reset_token: [u8; 16],
    },
    /// RFC 9000 §19.16.
    RetireConnectionId {
        /// Sequence number being retired.
        sequence_number: u64,
    },
    /// RFC 9000 §19.17.
    PathChallenge {
        /// 8-byte arbitrary challenge data.
        data: [u8; 8],
    },
    /// RFC 9000 §19.18.
    PathResponse {
        /// 8 bytes echoing the challenge.
        data: [u8; 8],
    },
    /// RFC 9000 §19.19 — CONNECTION_CLOSE (transport or
    /// application).
    ConnectionClose {
        /// Error code (transport or application namespace).
        error_code: u64,
        /// Originating frame type (only carried by transport-level
        /// closes — `0x1c` — and zero otherwise).
        frame_type: u64,
        /// `true` = application-level (`0x1d`); `false` =
        /// transport-level (`0x1c`).
        is_application: bool,
        /// Reason phrase (UTF-8; up to ~1KB in practice).
        reason: Vec<u8>,
    },
    /// RFC 9000 §19.20.
    HandshakeDone,
    /// RFC 9221 — unreliable datagram. Encoder always emits the
    /// length-prefixed form (`0x31`); decoder accepts both
    /// `0x30` (no length) and `0x31`.
    Datagram {
        /// Datagram payload.
        data: Vec<u8>,
    },
}

/// ECN counters carried by an ACK with type `0x03`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcnCounts {
    /// ECN-Capable Transport, codepoint 0.
    pub ect0: u64,
    /// ECN-Capable Transport, codepoint 1.
    pub ect1: u64,
    /// Congestion Experienced.
    pub ce: u64,
}

/// Decode one frame from the front of `buf`, advancing `buf` past
/// the consumed bytes. Returns the frame.
pub fn decode_frame(buf: &mut &[u8]) -> Result<Frame, Error> {
    let type_ = read_varint(buf)?;
    decode_frame_body(type_, buf)
}

fn decode_frame_body(type_: u64, buf: &mut &[u8]) -> Result<Frame, Error> {
    match type_ {
        0x00 => Ok(Frame::Padding),
        0x01 => Ok(Frame::Ping),
        0x02 | 0x03 => decode_ack(type_ == 0x03, buf),
        0x04 => Ok(Frame::ResetStream {
            stream_id: StreamId(read_varint(buf)?),
            application_error_code: read_varint(buf)?,
            final_size: read_varint(buf)?,
        }),
        0x05 => Ok(Frame::StopSending {
            stream_id: StreamId(read_varint(buf)?),
            application_error_code: read_varint(buf)?,
        }),
        0x06 => {
            let offset = read_varint(buf)?;
            let len = read_varint(buf)? as usize;
            let data = read_take(buf, len)?;
            Ok(Frame::Crypto { offset, data })
        }
        0x07 => {
            let len = read_varint(buf)? as usize;
            let token = read_take(buf, len)?;
            Ok(Frame::NewToken { token })
        }
        0x08..=0x0f => decode_stream(type_ as u8 - 0x08, buf),
        0x10 => Ok(Frame::MaxData(read_varint(buf)?)),
        0x11 => Ok(Frame::MaxStreamData {
            stream_id: StreamId(read_varint(buf)?),
            max_data: read_varint(buf)?,
        }),
        0x12 | 0x13 => Ok(Frame::MaxStreams {
            bidi: type_ == 0x12,
            max: read_varint(buf)?,
        }),
        0x14 => Ok(Frame::DataBlocked {
            data_limit: read_varint(buf)?,
        }),
        0x15 => Ok(Frame::StreamDataBlocked {
            stream_id: StreamId(read_varint(buf)?),
            stream_data_limit: read_varint(buf)?,
        }),
        0x16 | 0x17 => Ok(Frame::StreamsBlocked {
            bidi: type_ == 0x16,
            stream_limit: read_varint(buf)?,
        }),
        0x18 => {
            let sequence_number = read_varint(buf)?;
            let retire_prior_to = read_varint(buf)?;
            let cid_len = read_u8(buf)? as usize;
            if cid_len > 20 {
                return Err(Error::BadFrame);
            }
            let connection_id = read_take(buf, cid_len)?;
            let token_bytes = read_take(buf, 16)?;
            let mut stateless_reset_token = [0u8; 16];
            stateless_reset_token.copy_from_slice(&token_bytes);
            Ok(Frame::NewConnectionId {
                sequence_number,
                retire_prior_to,
                connection_id,
                stateless_reset_token,
            })
        }
        0x19 => Ok(Frame::RetireConnectionId {
            sequence_number: read_varint(buf)?,
        }),
        0x1a => Ok(Frame::PathChallenge {
            data: read_array8(buf)?,
        }),
        0x1b => Ok(Frame::PathResponse {
            data: read_array8(buf)?,
        }),
        0x1c | 0x1d => decode_connection_close(type_ == 0x1d, buf),
        0x1e => Ok(Frame::HandshakeDone),
        0x30 => {
            // No length — the rest of the buffer is the datagram.
            let data = buf.to_vec();
            *buf = &buf[buf.len()..];
            Ok(Frame::Datagram { data })
        }
        0x31 => {
            let len = read_varint(buf)? as usize;
            let data = read_take(buf, len)?;
            Ok(Frame::Datagram { data })
        }
        _ => Err(Error::BadFrame),
    }
}

fn decode_ack(with_ecn: bool, buf: &mut &[u8]) -> Result<Frame, Error> {
    let largest_acked = read_varint(buf)?;
    let ack_delay = read_varint(buf)?;
    let range_count = read_varint(buf)? as usize;
    let first_ack_range = read_varint(buf)?;
    // Each (gap, range) pair needs at least 2 varint bytes; cap
    // `range_count` by the remaining buffer length before allocating
    // so an attacker can't request a terabyte-sized Vec by claiming
    // a 62-bit `range_count` against a tiny buffer.
    if range_count > buf.len() {
        return Err(Error::BadFrame);
    }
    let mut ranges = Vec::with_capacity(range_count);
    for _ in 0..range_count {
        let gap = read_varint(buf)?;
        let range = read_varint(buf)?;
        ranges.push((gap, range));
    }
    let ecn = if with_ecn {
        Some(EcnCounts {
            ect0: read_varint(buf)?,
            ect1: read_varint(buf)?,
            ce: read_varint(buf)?,
        })
    } else {
        None
    };
    Ok(Frame::Ack {
        largest_acked,
        ack_delay,
        first_ack_range,
        ranges,
        ecn,
    })
}

fn decode_stream(bits: u8, buf: &mut &[u8]) -> Result<Frame, Error> {
    // Type byte low 3 bits: 0bOLN  → OFF / LEN / FIN.
    let fin = bits & 0b001 != 0;
    let has_len = bits & 0b010 != 0;
    let has_off = bits & 0b100 != 0;
    let stream_id = StreamId(read_varint(buf)?);
    let offset = if has_off { read_varint(buf)? } else { 0 };
    let data = if has_len {
        let len = read_varint(buf)? as usize;
        read_take(buf, len)?
    } else {
        // No length → rest of the packet payload.
        let v = buf.to_vec();
        *buf = &buf[buf.len()..];
        v
    };
    Ok(Frame::Stream {
        stream_id,
        offset,
        data,
        fin,
    })
}

fn decode_connection_close(is_application: bool, buf: &mut &[u8]) -> Result<Frame, Error> {
    let error_code = read_varint(buf)?;
    let frame_type = if is_application { 0 } else { read_varint(buf)? };
    let reason_len = read_varint(buf)? as usize;
    let reason = read_take(buf, reason_len)?;
    Ok(Frame::ConnectionClose {
        error_code,
        frame_type,
        is_application,
        reason,
    })
}

/// Borrowed-payload STREAM encoder — the hot-path twin of
/// [`encode_frame`]'s `Frame::Stream` arm (kept in lockstep
/// with it; same wire bytes, asserted by KAT). Skips the
/// owned `Frame` construction so a send-queue slice goes
/// straight into the packet plaintext with one copy.
pub(crate) fn encode_stream_frame_into(
    out: &mut Vec<u8>,
    stream_id: u64,
    offset: u64,
    data: &[u8],
    fin: bool,
) -> Result<(), Error> {
    // Always encode with LEN bit set so multiple STREAMs
    // can share one packet; OFF bit only when offset > 0.
    let has_off = offset > 0;
    let type_ = 0x08 | if has_off { 0b100 } else { 0 } | 0b010 | if fin { 0b001 } else { 0 };
    out.push(type_);
    put_varint(out, stream_id)?;
    if has_off {
        put_varint(out, offset)?;
    }
    put_varint(out, data.len() as u64)?;
    out.extend_from_slice(data);
    Ok(())
}

/// Encode `frame` to the back of `out`. Returns the number of
/// bytes appended.
pub fn encode_frame(frame: &Frame, out: &mut Vec<u8>) -> Result<usize, Error> {
    let start = out.len();
    match frame {
        Frame::Padding => out.push(0x00),
        Frame::Ping => out.push(0x01),
        Frame::Ack {
            largest_acked,
            ack_delay,
            first_ack_range,
            ranges,
            ecn,
        } => {
            out.push(if ecn.is_some() { 0x03 } else { 0x02 });
            put_varint(out, *largest_acked)?;
            put_varint(out, *ack_delay)?;
            put_varint(out, ranges.len() as u64)?;
            put_varint(out, *first_ack_range)?;
            for &(gap, range) in ranges {
                put_varint(out, gap)?;
                put_varint(out, range)?;
            }
            if let Some(ecn) = ecn {
                put_varint(out, ecn.ect0)?;
                put_varint(out, ecn.ect1)?;
                put_varint(out, ecn.ce)?;
            }
        }
        Frame::ResetStream {
            stream_id,
            application_error_code,
            final_size,
        } => {
            out.push(0x04);
            put_varint(out, stream_id.0)?;
            put_varint(out, *application_error_code)?;
            put_varint(out, *final_size)?;
        }
        Frame::StopSending {
            stream_id,
            application_error_code,
        } => {
            out.push(0x05);
            put_varint(out, stream_id.0)?;
            put_varint(out, *application_error_code)?;
        }
        Frame::Crypto { offset, data } => {
            out.push(0x06);
            put_varint(out, *offset)?;
            put_varint(out, data.len() as u64)?;
            out.extend_from_slice(data);
        }
        Frame::NewToken { token } => {
            out.push(0x07);
            put_varint(out, token.len() as u64)?;
            out.extend_from_slice(token);
        }
        Frame::Stream {
            stream_id,
            offset,
            data,
            fin,
        } => {
            // Always encode with LEN bit set so multiple STREAMs
            // can share one packet; OFF bit only when offset > 0.
            let has_off = *offset > 0;
            let type_ =
                0x08 | if has_off { 0b100 } else { 0 } | 0b010 | if *fin { 0b001 } else { 0 };
            out.push(type_);
            put_varint(out, stream_id.0)?;
            if has_off {
                put_varint(out, *offset)?;
            }
            put_varint(out, data.len() as u64)?;
            out.extend_from_slice(data);
        }
        Frame::MaxData(v) => {
            out.push(0x10);
            put_varint(out, *v)?;
        }
        Frame::MaxStreamData {
            stream_id,
            max_data,
        } => {
            out.push(0x11);
            put_varint(out, stream_id.0)?;
            put_varint(out, *max_data)?;
        }
        Frame::MaxStreams { bidi, max } => {
            out.push(if *bidi { 0x12 } else { 0x13 });
            put_varint(out, *max)?;
        }
        Frame::DataBlocked { data_limit } => {
            out.push(0x14);
            put_varint(out, *data_limit)?;
        }
        Frame::StreamDataBlocked {
            stream_id,
            stream_data_limit,
        } => {
            out.push(0x15);
            put_varint(out, stream_id.0)?;
            put_varint(out, *stream_data_limit)?;
        }
        Frame::StreamsBlocked { bidi, stream_limit } => {
            out.push(if *bidi { 0x16 } else { 0x17 });
            put_varint(out, *stream_limit)?;
        }
        Frame::NewConnectionId {
            sequence_number,
            retire_prior_to,
            connection_id,
            stateless_reset_token,
        } => {
            if connection_id.len() > 20 {
                return Err(Error::BadFrame);
            }
            out.push(0x18);
            put_varint(out, *sequence_number)?;
            put_varint(out, *retire_prior_to)?;
            out.push(connection_id.len() as u8);
            out.extend_from_slice(connection_id);
            out.extend_from_slice(stateless_reset_token);
        }
        Frame::RetireConnectionId { sequence_number } => {
            out.push(0x19);
            put_varint(out, *sequence_number)?;
        }
        Frame::PathChallenge { data } => {
            out.push(0x1a);
            out.extend_from_slice(data);
        }
        Frame::PathResponse { data } => {
            out.push(0x1b);
            out.extend_from_slice(data);
        }
        Frame::ConnectionClose {
            error_code,
            frame_type,
            is_application,
            reason,
        } => {
            out.push(if *is_application { 0x1d } else { 0x1c });
            put_varint(out, *error_code)?;
            if !*is_application {
                put_varint(out, *frame_type)?;
            }
            put_varint(out, reason.len() as u64)?;
            out.extend_from_slice(reason);
        }
        Frame::HandshakeDone => out.push(0x1e),
        Frame::Datagram { data } => {
            // Always emit length-prefixed form (0x31).
            out.push(0x31);
            put_varint(out, data.len() as u64)?;
            out.extend_from_slice(data);
        }
    }
    Ok(out.len() - start)
}

fn put_varint(out: &mut Vec<u8>, value: u64) -> Result<(), Error> {
    let mut buf = [0u8; 8];
    let n = write_varint(value, &mut buf)?;
    out.extend_from_slice(&buf[..n]);
    Ok(())
}

fn read_u8(buf: &mut &[u8]) -> Result<u8, Error> {
    let &[b, ref rest @ ..] = *buf else {
        return Err(Error::BadFrame);
    };
    *buf = rest;
    Ok(b)
}

fn read_take(buf: &mut &[u8], n: usize) -> Result<Vec<u8>, Error> {
    if buf.len() < n {
        return Err(Error::BadFrame);
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head.to_vec())
}

fn read_array8(buf: &mut &[u8]) -> Result<[u8; 8], Error> {
    if buf.len() < 8 {
        return Err(Error::BadFrame);
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(&buf[..8]);
    *buf = &buf[8..];
    Ok(out)
}

/// Return the size the shortest encoding of `value` will occupy
/// as a varint (helper for frame-size computation).
pub fn frame_varint_len(value: u64) -> usize {
    varint_len(value)
}

#[cfg(test)]
mod stream_fastpath_tests {
    //! Lockstep KAT: the borrowed-payload STREAM encoder
    //! must emit byte-identical wire to `encode_frame`'s
    //! `Frame::Stream` arm for every OFF/FIN/len shape.

    use super::*;
    use alloc::vec;

    #[test]
    fn encode_stream_frame_into_matches_encode_frame() {
        let payloads: [&[u8]; 3] = [b"", b"x", b"hello quic stream payload"];
        for sid in [0u64, 4, 63, 16384] {
            for offset in [0u64, 1, 65535] {
                for fin in [false, true] {
                    for payload in payloads {
                        let mut via_enum = Vec::new();
                        encode_frame(
                            &Frame::Stream {
                                stream_id: StreamId(sid),
                                offset,
                                data: payload.to_vec(),
                                fin,
                            },
                            &mut via_enum,
                        )
                        .unwrap();
                        let mut via_fast = vec![0xAA]; // pre-seed: must append, not clobber
                        encode_stream_frame_into(&mut via_fast, sid, offset, payload, fin).unwrap();
                        assert_eq!(via_fast[0], 0xAA);
                        assert_eq!(
                            &via_fast[1..],
                            &via_enum[..],
                            "sid={sid} off={offset} fin={fin} len={}",
                            payload.len()
                        );
                    }
                }
            }
        }
    }
}
