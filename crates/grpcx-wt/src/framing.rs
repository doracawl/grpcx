//! WebTransport stream + datagram framing — draft-ietf-webtrans-
//! http3 §4 + §5 (with RFC 9297 H/3 datagrams underneath).
//!
//! Two pure-function pairs:
//! - [`write_wt_stream_prefix`] / [`read_wt_stream_prefix`] —
//!   the leading two-varint sequence (`[0x54] [session_id]`) on
//!   every WT-typed H/3 uni- or bidi-stream.
//! - [`write_wt_datagram`] / [`read_wt_datagram`] — the
//!   `[quarter_stream_id] [payload]` wire shape of an RFC 9297
//!   H/3 datagram carrying WT bytes.

use crate::Error;
use crate::varint::{read_varint, varint_len, write_varint};
use alloc::vec::Vec;

/// WebTransport stream type byte (draft-ietf-webtrans-http3 §4).
pub const WT_STREAM_TYPE: u64 = 0x54;

// ============================================================
// Stream prefix.
// ============================================================

/// Emit the two-varint WebTransport stream prefix
/// (`[0x54] [session_id]`) into `out`. The caller writes user
/// bytes after this prefix on the same H/3 stream.
pub fn write_wt_stream_prefix(session_id: u64, out: &mut Vec<u8>) -> Result<(), Error> {
    put_varint(WT_STREAM_TYPE, out)?;
    put_varint(session_id, out)
}

/// Parse the WT stream prefix off the front of `buf`. Returns
/// `(session_id, bytes_consumed)`; the caller advances its
/// stream cursor by `bytes_consumed` to land on the first user
/// byte.
///
/// Errors with [`Error::BadStreamFrame`] if the leading varint
/// isn't `0x54` or the prefix is truncated.
pub fn read_wt_stream_prefix(buf: &[u8]) -> Result<(u64, usize), Error> {
    let mut cursor = buf;
    let type_ = read_varint(&mut cursor)?;
    if type_ != WT_STREAM_TYPE {
        return Err(Error::BadStreamFrame);
    }
    let session_id = read_varint(&mut cursor)?;
    Ok((session_id, buf.len() - cursor.len()))
}

// ============================================================
// Datagram.
// ============================================================

/// Emit a WT datagram into `out`: an RFC 9297 H/3 datagram
/// (`[quarter_stream_id] [payload]`) where
/// `quarter_stream_id = session_id / 4`.
///
/// Errors with [`Error::BadDatagram`] if `session_id % 4 != 0`
/// — the H/3 stream id of the bootstrap CONNECT must be on the
/// 4-stride class boundary (RFC 9000 §2.1 + RFC 9297 §2.1).
pub fn write_wt_datagram(session_id: u64, payload: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    if !session_id.is_multiple_of(4) {
        return Err(Error::BadDatagram);
    }
    let quarter = session_id / 4;
    put_varint(quarter, out).map_err(|_| Error::BadDatagram)?;
    out.extend_from_slice(payload);
    Ok(())
}

/// Parse a WT datagram off `buf`. Returns
/// `(session_id, payload_slice)` where `payload_slice` borrows
/// directly from `buf`.
///
/// Errors with [`Error::BadDatagram`] on a malformed varint.
pub fn read_wt_datagram(buf: &[u8]) -> Result<(u64, &[u8]), Error> {
    let mut cursor = buf;
    let quarter = read_varint(&mut cursor).map_err(|_| Error::BadDatagram)?;
    let consumed = buf.len() - cursor.len();
    let session_id = quarter.checked_mul(4).ok_or(Error::BadDatagram)?;
    Ok((session_id, &buf[consumed..]))
}

// ============================================================
// Helpers.
// ============================================================

fn put_varint(value: u64, out: &mut Vec<u8>) -> Result<(), Error> {
    let mut buf = [0u8; 8];
    let n = write_varint(value, &mut buf)?;
    out.extend_from_slice(&buf[..n]);
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn varint_size(v: u64) -> usize {
    varint_len(v)
}
