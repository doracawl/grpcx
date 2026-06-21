//! Blocking v2-frame I/O over `UnixStream`.
//!
//! Two entry points:
//! - [`write_frame`] — serialise a header + arbitrary
//!   payload slices into a **single** `writev(2)` (with
//!   a partial-write loop using
//!   `IoSlice::advance_slices`).
//! - [`read_frame`] — `read_exact` the 16-byte header,
//!   decode it, then `read_exact` the payload into a
//!   caller-owned `Vec<u8>` (caller reuses the buffer
//!   across calls — amortised allocation).
//!
//! Both functions are synchronous and blocking; they
//! match the client-side `UdsConnection` API shape.
//! The server side wraps the same primitives behind the
//! `grpcx-rt` reactor.
//!
//! T5.4 v2 note: an experimental buffered streaming
//! reader (`wire_v2::streaming::FrameReader`) was tried
//! to replace the two-syscall read_exact-header-then-
//! payload pattern with one buffered `read(2)` per
//! fill. On the lx64 reference run, UDS RTT regressed
//! from 5.7 µs → 8.0 µs (loopback kernel coalescing +
//! per-frame Vec alloc overhead outweighed the syscall
//! savings). The streaming reader stays available as a
//! module for future io_uring/custom-loop use; the v1
//! wire keeps the read_exact pattern that measured
//! best.

use alloc::vec::Vec;
use std::io::{IoSlice, Read, Write};
use std::os::unix::net::UnixStream;

use crate::RpcError;
use crate::wire_v2::{HEADER_LEN, Header, decode_header, encode_header};

/// Write one v2 frame: the 16-byte header followed by
/// the concatenation of `payload_parts`. The header's
/// `payload_len` MUST already equal the sum of the
/// non-empty parts' lengths — this function does not
/// recompute it.
///
/// One `writev(2)` syscall in the common case; falls
/// back to a partial-write loop on short writes.
pub(crate) fn write_frame(
    stream: &mut UnixStream,
    header: &Header,
    payload_parts: &[&[u8]],
) -> Result<(), RpcError> {
    let mut hdr_buf = [0u8; HEADER_LEN];
    encode_header(header, &mut hdr_buf);

    // Storage for the IoSlice array; lifetime tied to
    // `hdr_buf` and `payload_parts`. Capacity is one for
    // the header plus one per non-empty payload part.
    let mut slice_storage: Vec<IoSlice<'_>> = Vec::with_capacity(1 + payload_parts.len());
    slice_storage.push(IoSlice::new(&hdr_buf));
    for part in payload_parts {
        if !part.is_empty() {
            slice_storage.push(IoSlice::new(part));
        }
    }
    let mut slices: &mut [IoSlice<'_>] = &mut slice_storage;

    while !slices.is_empty() {
        let n = stream
            .write_vectored(slices)
            .map_err(|_| RpcError::Transport("uds: write_vectored failed"))?;
        if n == 0 {
            return Err(RpcError::Transport("uds: write returned 0 (peer closed)"));
        }
        IoSlice::advance_slices(&mut slices, n);
    }
    Ok(())
}

/// Read one v2 frame: 16-byte header + `payload_len`
/// payload bytes into `recv_buf`. `recv_buf` is
/// `clear()`ed-then-resized to hold the payload; callers
/// reuse it across frames to amortise allocation.
///
/// Returns the decoded [`Header`]; the payload bytes
/// live in `recv_buf[..header.payload_len as usize]`.
pub(crate) fn read_frame(
    stream: &mut UnixStream,
    recv_buf: &mut Vec<u8>,
) -> Result<Header, RpcError> {
    let mut hdr = [0u8; HEADER_LEN];
    stream
        .read_exact(&mut hdr)
        .map_err(|_| RpcError::Transport("uds: read header failed"))?;
    let header = decode_header(&hdr).map_err(|_| RpcError::Transport("uds: invalid v2 header"))?;
    let len = header.payload_len as usize;
    recv_buf.clear();
    recv_buf.resize(len, 0);
    if len > 0 {
        stream
            .read_exact(&mut recv_buf[..len])
            .map_err(|_| RpcError::Transport("uds: read payload failed"))?;
    }
    Ok(header)
}
