//! Blocking v2-frame I/O over `TcpStream`.
//!
//! Mirrors [`crate::transport_uds::wire`] line-for-line
//! against the TCP socket. Two entry points:
//!
//! - [`write_frame`] — serialise a header + arbitrary
//!   payload slices into a **single** `writev(2)` (with
//!   a partial-write loop using
//!   `IoSlice::advance_slices`).
//! - [`read_frame`] — `read_exact` the 16-byte header,
//!   decode it, then `read_exact` the payload into a
//!   caller-owned `Vec<u8>` (caller reuses the buffer
//!   across calls — amortised allocation).

use alloc::vec::Vec;
use std::io::{IoSlice, Read, Write};
use std::net::TcpStream;

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
    stream: &mut TcpStream,
    header: &Header,
    payload_parts: &[&[u8]],
) -> Result<(), RpcError> {
    let mut hdr_buf = [0u8; HEADER_LEN];
    encode_header(header, &mut hdr_buf);

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
            .map_err(|_| RpcError::Transport("tcp-mux-v2: write_vectored failed"))?;
        if n == 0 {
            return Err(RpcError::Transport(
                "tcp-mux-v2: write returned 0 (peer closed)",
            ));
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
    stream: &mut TcpStream,
    recv_buf: &mut Vec<u8>,
) -> Result<Header, RpcError> {
    let mut hdr = [0u8; HEADER_LEN];
    stream
        .read_exact(&mut hdr)
        .map_err(|_| RpcError::Transport("tcp-mux-v2: read header failed"))?;
    let header =
        decode_header(&hdr).map_err(|_| RpcError::Transport("tcp-mux-v2: invalid v2 header"))?;
    let len = header.payload_len as usize;
    recv_buf.clear();
    recv_buf.resize(len, 0);
    if len > 0 {
        stream
            .read_exact(&mut recv_buf[..len])
            .map_err(|_| RpcError::Transport("tcp-mux-v2: read payload failed"))?;
    }
    Ok(header)
}

/// Same as [`read_frame`] but assumes the socket is in
/// non-blocking mode and **busy-polls** on `WouldBlock`
/// instead of blocking in the kernel. Used by
/// `Server::listen_tcp_mux_v2_busy_poll_until`'s serve
/// loop to eliminate the per-RPC `read(2)` syscall stall
/// that bounds the < 8 µs gate from below on Linux.
///
/// Burns one CPU per active connection on the server.
pub(crate) fn read_frame_busy_poll(
    stream: &mut TcpStream,
    recv_buf: &mut Vec<u8>,
) -> Result<Header, RpcError> {
    let mut hdr = [0u8; HEADER_LEN];
    read_exact_busy_poll(stream, &mut hdr)
        .map_err(|_| RpcError::Transport("tcp-mux-v2: read header failed"))?;
    let header =
        decode_header(&hdr).map_err(|_| RpcError::Transport("tcp-mux-v2: invalid v2 header"))?;
    let len = header.payload_len as usize;
    recv_buf.clear();
    recv_buf.resize(len, 0);
    if len > 0 {
        read_exact_busy_poll(stream, &mut recv_buf[..len])
            .map_err(|_| RpcError::Transport("tcp-mux-v2: read payload failed"))?;
    }
    Ok(header)
}

fn read_exact_busy_poll(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        match stream.read(&mut buf[off..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => off += n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                core::hint::spin_loop();
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
