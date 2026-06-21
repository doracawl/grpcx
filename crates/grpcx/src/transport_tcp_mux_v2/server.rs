//! Server-side `transport-tcp-mux v2` —
//! `Server::listen_tcp_mux_v2` accept loop + per-
//! connection thread that drains v2 frames, dispatches
//! to registered `Service` impls through
//! [`Service::dispatch_with_ctx`], and writes response
//! frames back.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::connection::set_socket_buf;
use super::wire::{read_frame, read_frame_busy_poll, write_frame};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
use crate::{CallContext, Metadata, RpcError, Server, Status, StatusCode};

const PSEUDO_SERVICE: &[u8] = b":service";
const PSEUDO_METHOD: &[u8] = b":method";
const PSEUDO_KIND: &[u8] = b":kind";
const PSEUDO_STATUS: &[u8] = b":status";

/// Server-side accepted `transport-tcp-mux v2`
/// connection. Created internally by the
/// `Server::listen_tcp_mux_v2*` accept loop; not part of
/// the public API today (constructor and `serve` are
/// crate-private). Reserved as a `pub` surface so
/// `accept_tcp_mux_v2` can grow as a sans-IO API in
/// v1.x.
pub struct TcpMuxV2ServerConn {
    stream: TcpStream,
    recv_buf: Vec<u8>,
    streams: BTreeMap<u32, StreamState>,
}

struct StreamState {
    service: String,
    method: String,
    user_meta: Metadata,
    request_bodies: Vec<Vec<u8>>,
}

impl TcpMuxV2ServerConn {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            recv_buf: Vec::with_capacity(64 * 1024),
            streams: BTreeMap::new(),
        }
    }

    fn serve(&mut self, server: &Server) -> Result<(), RpcError> {
        self.serve_inner(server, false)
    }

    fn serve_busy_poll(&mut self, server: &Server) -> Result<(), RpcError> {
        self.stream
            .set_nonblocking(true)
            .map_err(|_| RpcError::Transport("tcp-mux-v2: set_nonblocking failed"))?;
        self.serve_inner(server, true)
    }

    fn serve_inner(&mut self, server: &Server, busy_poll: bool) -> Result<(), RpcError> {
        loop {
            let read = if busy_poll {
                read_frame_busy_poll(&mut self.stream, &mut self.recv_buf)
            } else {
                read_frame(&mut self.stream, &mut self.recv_buf)
            };
            let header = match read {
                Ok(h) => h,
                // Peer closed mid-stream — exit cleanly.
                Err(_) => return Ok(()),
            };
            match header.frame_type {
                FrameType::Begin => self.handle_begin(server, header)?,
                FrameType::Data => self.handle_data(server, header)?,
                FrameType::End => {
                    if let Some(st) = self.streams.remove(&header.stream_id) {
                        self.dispatch_stream(server, header.stream_id, st)?;
                    } else {
                        self.send_reset(
                            header.stream_id,
                            StatusCode::Internal,
                            "END on unknown stream",
                        )?;
                    }
                }
                FrameType::Reset => {
                    self.streams.remove(&header.stream_id);
                }
                FrameType::Ping => {
                    let payload = self.recv_buf.clone();
                    let pong = Header {
                        stream_id: 0,
                        frame_type: FrameType::Pong,
                        flags: Flags::NONE,
                        metadata_count: 0,
                        payload_len: payload.len() as u32,
                    };
                    write_frame(&mut self.stream, &pong, &[&payload])?;
                }
                FrameType::Pong => {
                    // Stray PONG (we did not send PING) — ignore.
                }
            }
        }
    }

    fn handle_begin(&mut self, server: &Server, header: Header) -> Result<(), RpcError> {
        let payload_len = header.payload_len as usize;
        let payload = &self.recv_buf[..payload_len];
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
            return self.send_reset(
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
            self.dispatch_stream(server, header.stream_id, st)?;
        } else {
            self.streams.insert(header.stream_id, st);
        }
        Ok(())
    }

    fn handle_data(&mut self, server: &Server, header: Header) -> Result<(), RpcError> {
        let payload_len = header.payload_len as usize;
        let body_owned: Vec<u8> = self.recv_buf[..payload_len].to_vec();

        let dispatch_now = if let Some(st) = self.streams.get_mut(&header.stream_id) {
            if !body_owned.is_empty() {
                st.request_bodies.push(body_owned);
            }
            header.flags.contains(Flags::END_STREAM)
        } else {
            return self.send_reset(
                header.stream_id,
                StatusCode::Internal,
                "DATA on unknown stream",
            );
        };

        if dispatch_now {
            let st = self.streams.remove(&header.stream_id).unwrap();
            self.dispatch_stream(server, header.stream_id, st)?;
        }
        Ok(())
    }

    fn dispatch_stream(
        &mut self,
        server: &Server,
        stream_id: u32,
        st: StreamState,
    ) -> Result<(), RpcError> {
        let svc = match server.find_service(&st.service) {
            Some(s) => s,
            None => {
                return self.send_reset(
                    stream_id,
                    StatusCode::Unimplemented,
                    "service not registered",
                );
            }
        };

        let mut ctx = CallContext::new(st.user_meta);
        // TCP carries no kernel-attested peer credentials —
        // ctx.peer_cred() stays None on this transport.

        match svc.dispatch_with_ctx(&st.method, &mut ctx, &st.request_bodies) {
            Ok(resp_bodies) => {
                let trailing = ctx.into_trailing();
                self.send_response(stream_id, resp_bodies, trailing)
            }
            Err(RpcError::Status(s)) => self.send_reset_status(stream_id, s),
            Err(_) => self.send_reset(stream_id, StatusCode::Internal, "handler error"),
        }
    }

    fn send_response(
        &mut self,
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
            return write_frame(&mut self.stream, &header, &[&meta_buf, &body]);
        }

        for body in &bodies {
            let header = Header {
                stream_id,
                frame_type: FrameType::Data,
                flags: Flags::NONE,
                metadata_count: 0,
                payload_len: body.len() as u32,
            };
            write_frame(&mut self.stream, &header, &[body])?;
        }
        let end_header = Header {
            stream_id,
            frame_type: FrameType::End,
            flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
            metadata_count: trailing_entries.len() as u16,
            payload_len: meta_len as u32,
        };
        write_frame(&mut self.stream, &end_header, &[&meta_buf])
    }

    fn send_reset(&mut self, stream_id: u32, code: StatusCode, msg: &str) -> Result<(), RpcError> {
        let payload = encode_reset_payload(code, msg);
        let header = Header {
            stream_id,
            frame_type: FrameType::Reset,
            flags: Flags::END_STREAM,
            metadata_count: 0,
            payload_len: payload.len() as u32,
        };
        write_frame(&mut self.stream, &header, &[&payload])
    }

    fn send_reset_status(&mut self, stream_id: u32, status: Status) -> Result<(), RpcError> {
        self.send_reset(stream_id, status.code, &status.message)
    }
}

/// Per-connection entry point. Linux + the opt-in
/// `transport-tcp-mux-iouring` feature route the serve
/// loop through the io_uring fast paths:
///
/// 1. v1.1 `linux_iouring_v2` — SQPOLL + cqe event loop
///    + linked-send pair (RFC `_dev/RFC-v1.1-iouring-sqpoll-server.md`).
/// 2. v1   `linux_iouring`    — per-connection ring with
///    submit-and-wait per syscall (T4.2). Lives on as a
///    fallback while v1.1 hardens.
/// 3. T4.1 blocking accept-thread path.
///
/// Each falls back to the next on setup failure (kernel
/// too old, EPERM, seccomp). Non-Linux / feature-off
/// builds compile to just the blocking branch.
#[allow(unused_mut)]
fn serve_one(stream: TcpStream, server: &Server) {
    let mut stream = stream;
    #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
    {
        match super::linux_iouring_v2::try_serve_iouring_v2(stream, server) {
            Ok(()) => return,
            Err((s, _v2_err)) => {
                stream = s;
            }
        }
        match super::linux_iouring::try_serve_iouring(stream, server) {
            Ok(()) => return,
            Err((s, _v1_err)) => {
                stream = s;
            }
        }
    }
    let mut conn = TcpMuxV2ServerConn::new(stream);
    let _ = conn.serve(server);
}

/// Busy-poll serve path: bypasses the io_uring SQPOLL +
/// linux_iouring fallbacks (those carry their own
/// kernel-side scheduling overhead) and runs the wire
/// loop directly on the accepting thread with a
/// non-blocking socket. Each active connection burns one
/// CPU; the per-connection idle cost is "scales with
/// connection count, not RPC rate" — same trade-off the
/// PLAN deferred to the deployer in v1.2.1.
fn serve_one_busy_poll(stream: TcpStream, server: &Server) {
    let mut conn = TcpMuxV2ServerConn::new(stream);
    let _ = conn.serve_busy_poll(server);
}

fn encode_reset_payload(code: StatusCode, msg: &str) -> Vec<u8> {
    let msg_bytes = msg.as_bytes();
    let mut out = Vec::with_capacity(8 + msg_bytes.len());
    out.extend_from_slice(&(code as u32).to_le_bytes());
    out.extend_from_slice(&(msg_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(msg_bytes);
    out
}

// ============================================================
// Server entry points
// ============================================================

impl Server {
    /// Bind to `addr` (`"host:port"`) and serve
    /// registered services over `transport-tcp-mux v2`.
    /// Blocks until the server is shut down (does not
    /// return — wrap in a thread).
    pub fn listen_tcp_mux_v2(&self, addr: &str) -> Result<(), RpcError> {
        let stop = AtomicBool::new(false);
        self.listen_tcp_mux_v2_until(addr, &stop)
    }

    /// As [`Self::listen_tcp_mux_v2`], but exits cleanly
    /// when `stop` flips. Each accepted connection runs
    /// on its own OS thread and finishes when its peer
    /// closes the socket.
    pub fn listen_tcp_mux_v2_until(&self, addr: &str, stop: &AtomicBool) -> Result<(), RpcError> {
        self.listen_tcp_mux_v2_inner(addr, stop, false)
    }

    /// Same as [`Self::listen_tcp_mux_v2_until`] but
    /// every connection worker runs in **user-space
    /// busy-poll mode**: the socket is set non-blocking
    /// and the serve loop spins on `WouldBlock` instead
    /// of paying a `read(2)` syscall per RPC. Bypasses
    /// the SQPOLL + linux_iouring fallback paths
    /// completely.
    ///
    /// This is the deployer-opt-in path the PLAN
    /// `v1.2.1` discussion mentions: closing the < 8 µs
    /// gate on lx64 requires *server-side* busy-poll,
    /// which scales server CPU with connection count
    /// rather than RPC rate. Use only when latency is
    /// worth the cost; default to
    /// [`Self::listen_tcp_mux_v2_until`] otherwise.
    pub fn listen_tcp_mux_v2_busy_poll_until(
        &self,
        addr: &str,
        stop: &AtomicBool,
    ) -> Result<(), RpcError> {
        self.listen_tcp_mux_v2_inner(addr, stop, true)
    }

    fn listen_tcp_mux_v2_inner(
        &self,
        addr: &str,
        stop: &AtomicBool,
        busy_poll: bool,
    ) -> Result<(), RpcError> {
        let listener =
            TcpListener::bind(addr).map_err(|_| RpcError::Transport("tcp-mux-v2: bind failed"))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| RpcError::Transport("tcp-mux-v2: listener set_nonblocking failed"))?;

        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    if !busy_poll && let Err(e) = stream.set_nonblocking(false) {
                        let _ = e;
                    }
                    // TCP_NODELAY on the accepted socket
                    // for the same reason the client sets
                    // it — server-side END frames want
                    // out the wire immediately.
                    let _ = stream.set_nodelay(true);
                    let fd = stream.as_raw_fd();
                    set_socket_buf(fd, true, 16 * 1024 * 1024);
                    set_socket_buf(fd, false, 16 * 1024 * 1024);
                    let server = self.clone();
                    let h = std::thread::spawn(move || {
                        if busy_poll {
                            serve_one_busy_poll(stream, &server);
                        } else {
                            serve_one(stream, &server);
                        }
                    });
                    handles.push(h);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(RpcError::Transport("tcp-mux-v2: accept failed")),
            }
            handles.retain(|h| !h.is_finished());
        }
        Ok(())
    }
}
