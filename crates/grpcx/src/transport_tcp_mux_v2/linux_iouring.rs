//! Linux io_uring fast path for the wire-v2 tcp-mux
//! server (T4.2 of `_dev/IMPLEMENTATION-PLAN.md`).
//!
//! Drop-in replacement for [`super::server::TcpMuxV2ServerConn::serve`]
//! that routes the same frame state machine through
//! `grpcx_rt::iouring` primitives instead of blocking
//! `read_exact` / `write_vectored`. Same `BEGIN` /
//! `DATA` / `END` / `RESET` semantics, same dispatch
//! into `Service::dispatch_with_ctx`.
//!
//! Per-connection ring discipline (v1 wiring): one
//! `UringRing` per accepted socket, no SQPOLL by default
//! (avoids `EPERM` on locked-down kernels), no
//! multishot recv. The new primitives — SQPOLL,
//! `IORING_REGISTER_FILES`, multishot recv, linked-send
//! pairs — are all reachable through this serve path
//! and the matching ops, but the v1 wiring uses the
//! simpler submit-and-wait submission pattern so that a
//! single misuse of multishot doesn't take the entire
//! Linux fast path down. The bench-and-tune pass under
//! T5.4 will turn on SQPOLL + per-thread rings once a
//! Linux runner is available.
//!
//! Gated on `cfg(all(target_os = "linux", feature =
//! "transport-tcp-mux-iouring"))`; on non-Linux or
//! without the feature the serve loop in `server.rs`
//! handles the connection through the T4.1 blocking
//! accept-thread path.

#![cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use std::net::TcpStream;
use std::os::fd::AsRawFd;

use grpcx_rt::iouring::{UringRing, drain_cqes, submit_recv, submit_send};

use crate::wire_v2::{
    Flags, FrameType, HEADER_LEN, Header, MetadataEntry, block_wire_len, decode_header,
    decode_metadata_block, encode_header, encode_metadata_block,
};
use crate::{CallContext, Metadata, RpcError, Server, Status, StatusCode};

const PSEUDO_SERVICE: &[u8] = b":service";
const PSEUDO_METHOD: &[u8] = b":method";
const PSEUDO_KIND: &[u8] = b":kind";
const PSEUDO_STATUS: &[u8] = b":status";

/// User-data cookie discriminator: low byte distinguishes
/// SEND vs RECV completions; the rest is a per-op tag the
/// caller can ignore (we drive with submit-and-wait so
/// there's only one outstanding op at a time).
const UD_RECV: u64 = 0x01;
const UD_SEND: u64 = 0x02;

struct StreamState {
    service: String,
    method: String,
    user_meta: Metadata,
    request_bodies: Vec<Vec<u8>>,
}

/// Per-connection serve loop driven by io_uring.
///
/// Two failure shapes:
/// - `Err((stream, err))` — the io_uring ring failed to
///   set up (kernel < 5.1, EPERM, seccomp ban). The
///   stream comes back to the caller so the blocking
///   T4.1 path can take over without losing the
///   connection.
/// - `Ok(())` after a clean peer close, or after a
///   protocol violation handled by sending a RESET. The
///   stream is consumed in either case.
///
/// Any I/O failure **after** ring setup is treated as a
/// connection-scoped abort: the function returns `Ok(())`
/// and lets the connection drop. The listener stays up.
pub(super) fn try_serve_iouring(
    stream: TcpStream,
    server: &Server,
) -> Result<(), (TcpStream, RpcError)> {
    let fd = stream.as_raw_fd();
    let mut ring = match UringRing::new(64) {
        Ok(r) => r,
        Err(_) => {
            return Err((
                stream,
                RpcError::Transport("tcp-mux-v2: io_uring setup failed"),
            ));
        }
    };
    let mut recv_buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut streams: BTreeMap<u32, StreamState> = BTreeMap::new();

    let _ = drive_loop(&mut ring, fd, server, &mut recv_buf, &mut streams);
    Ok(())
}

fn drive_loop(
    ring: &mut UringRing,
    fd: i32,
    server: &Server,
    recv_buf: &mut Vec<u8>,
    streams: &mut BTreeMap<u32, StreamState>,
) -> Result<(), RpcError> {
    loop {
        let header = match read_frame(ring, fd, recv_buf) {
            Ok(h) => h,
            // Peer closed mid-stream — exit cleanly.
            Err(_) => return Ok(()),
        };
        match header.frame_type {
            FrameType::Begin => handle_begin(ring, fd, server, streams, header, recv_buf)?,
            FrameType::Data => handle_data(ring, fd, server, streams, header, recv_buf)?,
            FrameType::End => {
                if let Some(st) = streams.remove(&header.stream_id) {
                    dispatch_stream(ring, fd, server, header.stream_id, st)?;
                } else {
                    send_reset(
                        ring,
                        fd,
                        header.stream_id,
                        StatusCode::Internal,
                        "END on unknown stream",
                    )?;
                }
            }
            FrameType::Reset => {
                streams.remove(&header.stream_id);
            }
            FrameType::Ping => {
                let payload = recv_buf.clone();
                let pong = Header {
                    stream_id: 0,
                    frame_type: FrameType::Pong,
                    flags: Flags::NONE,
                    metadata_count: 0,
                    payload_len: payload.len() as u32,
                };
                write_frame(ring, fd, &pong, &[&payload])?;
            }
            FrameType::Pong => {
                // Stray PONG — ignore.
            }
        }
    }
}

/// Read one v2 frame from `fd` via io_uring: 16-byte
/// header + `payload_len` payload bytes.
fn read_frame(ring: &mut UringRing, fd: i32, recv_buf: &mut Vec<u8>) -> Result<Header, RpcError> {
    let mut hdr = [0u8; HEADER_LEN];
    read_exact_iouring(ring, fd, &mut hdr)?;
    let header =
        decode_header(&hdr).map_err(|_| RpcError::Transport("tcp-mux-v2: invalid v2 header"))?;
    let len = header.payload_len as usize;
    recv_buf.clear();
    recv_buf.resize(len, 0);
    if len > 0 {
        read_exact_iouring(ring, fd, &mut recv_buf[..len])?;
    }
    Ok(header)
}

/// Drive io_uring's RECV op until `buf` is filled. Each
/// submission consumes up to `buf.len()` bytes; the loop
/// re-submits for any remainder. A short read of zero is
/// peer-close.
fn read_exact_iouring(ring: &mut UringRing, fd: i32, buf: &mut [u8]) -> Result<(), RpcError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        // SAFETY: `buf` outlives `submit_and_wait` +
        // `drain_cqes` below; no other writer races us;
        // the slice is mutable for the remaining range.
        unsafe {
            submit_recv(
                ring,
                fd,
                buf[filled..].as_mut_ptr(),
                (buf.len() - filled) as u32,
                0,
                UD_RECV,
            )
            .map_err(|_| RpcError::Transport("tcp-mux-v2: io_uring recv submit failed"))?;
        }
        let _ = ring
            .submit_and_wait(1, 1)
            .map_err(|_| RpcError::Transport("tcp-mux-v2: io_uring recv wait failed"))?;
        let mut res: Option<i32> = None;
        drain_cqes(ring, |c| {
            if c.user_data == UD_RECV {
                res = Some(c.res);
            }
        });
        let n = res.ok_or(RpcError::Transport("tcp-mux-v2: io_uring recv missing CQE"))?;
        if n < 0 {
            return Err(RpcError::Transport("tcp-mux-v2: io_uring recv errored"));
        }
        if n == 0 {
            return Err(RpcError::Transport("tcp-mux-v2: peer closed"));
        }
        filled += n as usize;
    }
    Ok(())
}

/// Write one v2 frame via io_uring: header then each
/// non-empty payload part as a separate SEND. The frame
/// boundary is preserved by submission order on a single
/// connected socket.
fn write_frame(
    ring: &mut UringRing,
    fd: i32,
    header: &Header,
    payload_parts: &[&[u8]],
) -> Result<(), RpcError> {
    let mut hdr_buf = [0u8; HEADER_LEN];
    encode_header(header, &mut hdr_buf);
    send_all_iouring(ring, fd, &hdr_buf)?;
    for part in payload_parts {
        if !part.is_empty() {
            send_all_iouring(ring, fd, part)?;
        }
    }
    Ok(())
}

/// Drive io_uring's SEND op until `buf` is fully drained.
fn send_all_iouring(ring: &mut UringRing, fd: i32, buf: &[u8]) -> Result<(), RpcError> {
    let mut sent = 0usize;
    while sent < buf.len() {
        // SAFETY: `buf` outlives `submit_and_wait` +
        // `drain_cqes`; we only read from it.
        unsafe {
            submit_send(
                ring,
                fd,
                buf[sent..].as_ptr(),
                (buf.len() - sent) as u32,
                0,
                UD_SEND,
            )
            .map_err(|_| RpcError::Transport("tcp-mux-v2: io_uring send submit failed"))?;
        }
        let _ = ring
            .submit_and_wait(1, 1)
            .map_err(|_| RpcError::Transport("tcp-mux-v2: io_uring send wait failed"))?;
        let mut res: Option<i32> = None;
        drain_cqes(ring, |c| {
            if c.user_data == UD_SEND {
                res = Some(c.res);
            }
        });
        let n = res.ok_or(RpcError::Transport("tcp-mux-v2: io_uring send missing CQE"))?;
        if n <= 0 {
            return Err(RpcError::Transport("tcp-mux-v2: io_uring send errored"));
        }
        sent += n as usize;
    }
    Ok(())
}

// ------------------------------------------------------------
// Frame state machine — mirrors `server.rs`. Kept here as
// a separate copy so the io_uring path stays self-contained
// (a shared trait would force lifetime gymnastics on the
// blocking path that wins nothing).
// ------------------------------------------------------------

fn handle_begin(
    ring: &mut UringRing,
    fd: i32,
    server: &Server,
    streams: &mut BTreeMap<u32, StreamState>,
    header: Header,
    recv_buf: &[u8],
) -> Result<(), RpcError> {
    let payload_len = header.payload_len as usize;
    let payload = &recv_buf[..payload_len];
    let (entries, body) = if header.flags.contains(Flags::HAS_METADATA) {
        decode_metadata_block(payload, header.metadata_count)
            .map_err(|_| RpcError::Codec("tcp-mux-v2: metadata block decode failed"))?
    } else {
        (Vec::new(), payload)
    };

    let mut service = String::new();
    let mut method = String::new();
    let mut user_meta = Metadata::default();
    for e in &entries {
        if e.name == PSEUDO_SERVICE {
            service = String::from_utf8_lossy(e.value).into_owned();
        } else if e.name == PSEUDO_METHOD {
            method = String::from_utf8_lossy(e.value).into_owned();
        } else if e.name == PSEUDO_KIND {
            // SPEC § 4.1 — `:kind` is informational.
        } else {
            user_meta.headers.push((
                String::from_utf8_lossy(e.name).into_owned(),
                e.value.to_vec(),
            ));
        }
    }

    if service.is_empty() || method.is_empty() {
        return send_reset(
            ring,
            fd,
            header.stream_id,
            StatusCode::Internal,
            "BEGIN missing :service or :method",
        );
    }

    let mut st = StreamState {
        service,
        method,
        user_meta,
        request_bodies: Vec::new(),
    };
    if !body.is_empty() {
        st.request_bodies.push(body.to_vec());
    }

    if header.flags.contains(Flags::END_STREAM) {
        dispatch_stream(ring, fd, server, header.stream_id, st)?;
    } else {
        streams.insert(header.stream_id, st);
    }
    Ok(())
}

fn handle_data(
    ring: &mut UringRing,
    fd: i32,
    server: &Server,
    streams: &mut BTreeMap<u32, StreamState>,
    header: Header,
    recv_buf: &[u8],
) -> Result<(), RpcError> {
    let payload_len = header.payload_len as usize;
    let body_owned: Vec<u8> = recv_buf[..payload_len].to_vec();

    let dispatch_now = if let Some(st) = streams.get_mut(&header.stream_id) {
        if !body_owned.is_empty() {
            st.request_bodies.push(body_owned);
        }
        header.flags.contains(Flags::END_STREAM)
    } else {
        return send_reset(
            ring,
            fd,
            header.stream_id,
            StatusCode::Internal,
            "DATA on unknown stream",
        );
    };

    if dispatch_now {
        let st = streams.remove(&header.stream_id).unwrap();
        dispatch_stream(ring, fd, server, header.stream_id, st)?;
    }
    Ok(())
}

fn dispatch_stream(
    ring: &mut UringRing,
    fd: i32,
    server: &Server,
    stream_id: u32,
    st: StreamState,
) -> Result<(), RpcError> {
    let svc = match server.find_service(&st.service) {
        Some(s) => s,
        None => {
            return send_reset(
                ring,
                fd,
                stream_id,
                StatusCode::Unimplemented,
                "service not registered",
            );
        }
    };

    let mut ctx = CallContext::new(st.user_meta);
    // TCP carries no kernel-attested peer credentials.

    match svc.dispatch_with_ctx(&st.method, &mut ctx, &st.request_bodies) {
        Ok(resp_bodies) => {
            let trailing = ctx.into_trailing();
            send_response(ring, fd, stream_id, resp_bodies, trailing)
        }
        Err(RpcError::Status(s)) => send_reset_status(ring, fd, stream_id, s),
        Err(_) => send_reset(ring, fd, stream_id, StatusCode::Internal, "handler error"),
    }
}

fn send_response(
    ring: &mut UringRing,
    fd: i32,
    stream_id: u32,
    bodies: Vec<Vec<u8>>,
    trailing: Metadata,
) -> Result<(), RpcError> {
    let status_ok = [0u8; 4];
    let mut trailing_entries: Vec<MetadataEntry<'_>> =
        Vec::with_capacity(1 + trailing.headers.len());
    trailing_entries.push(MetadataEntry {
        name: PSEUDO_STATUS,
        value: &status_ok,
    });
    for (n, v) in &trailing.headers {
        trailing_entries.push(MetadataEntry {
            name: n.as_bytes(),
            value: v.as_slice(),
        });
    }
    let meta_len = block_wire_len(&trailing_entries);
    let mut meta_buf = alloc::vec![0u8; meta_len];
    encode_metadata_block(&trailing_entries, &mut meta_buf);

    if bodies.len() <= 1 {
        let body = bodies.into_iter().next().unwrap_or_default();
        let header = Header {
            stream_id,
            frame_type: FrameType::End,
            flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
            metadata_count: trailing_entries.len() as u16,
            payload_len: (meta_len + body.len()) as u32,
        };
        return write_frame(ring, fd, &header, &[&meta_buf, &body]);
    }

    for body in &bodies {
        let header = Header {
            stream_id,
            frame_type: FrameType::Data,
            flags: Flags::NONE,
            metadata_count: 0,
            payload_len: body.len() as u32,
        };
        write_frame(ring, fd, &header, &[body])?;
    }
    let end_header = Header {
        stream_id,
        frame_type: FrameType::End,
        flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
        metadata_count: trailing_entries.len() as u16,
        payload_len: meta_len as u32,
    };
    write_frame(ring, fd, &end_header, &[&meta_buf])
}

fn send_reset(
    ring: &mut UringRing,
    fd: i32,
    stream_id: u32,
    code: StatusCode,
    msg: &str,
) -> Result<(), RpcError> {
    let payload = encode_reset_payload(code, msg);
    let header = Header {
        stream_id,
        frame_type: FrameType::Reset,
        flags: Flags::END_STREAM,
        metadata_count: 0,
        payload_len: payload.len() as u32,
    };
    write_frame(ring, fd, &header, &[&payload])
}

fn send_reset_status(
    ring: &mut UringRing,
    fd: i32,
    stream_id: u32,
    status: Status,
) -> Result<(), RpcError> {
    send_reset(ring, fd, stream_id, status.code, &status.message)
}

fn encode_reset_payload(code: StatusCode, msg: &str) -> Vec<u8> {
    let msg_bytes = msg.as_bytes();
    let mut out = Vec::with_capacity(8 + msg_bytes.len());
    out.extend_from_slice(&(code as u32).to_le_bytes());
    out.extend_from_slice(&(msg_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(msg_bytes);
    out
}
