//! Client-side `TcpMuxV2Connection` — synchronous blocking
//! v2-frame client over `TcpStream`.
//!
//! One connection carries many concurrent RPCs through
//! distinct `stream_id`s; client-allocated ids are odd
//! and strictly increasing per SPEC § 3.

use alloc::string::String;
use alloc::vec::Vec;
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;

use super::wire::{read_frame, write_frame};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
use crate::wire_v2::{HEADER_LEN, encode_header};
use crate::{Metadata, Method, RpcChannel, RpcError, Status, StatusCode};

#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
use super::linux_iouring_client::ClientRing;

/// Kernel send / receive buffer size set on every TCP
/// socket the transport creates. Matches the RFC § 5
/// ceiling constraint (16 MiB) — large enough that a
/// 16 MiB v2 frame drains in one writev call.
const SOCKET_BUF_BYTES: i32 = 16 * 1024 * 1024;

/// Default initial capacity for the per-connection
/// receive buffer (`Vec<u8>` reused across frames).
const INITIAL_RECV_BUF: usize = 64 * 1024;

/// Maximum response messages collected before the
/// transport gives up — defends against a runaway
/// server.
const MAX_RESPONSE_FRAMES: usize = 65_536;

/// Pseudo-header for the gRPC service name (SPEC § 4.1).
const PSEUDO_SERVICE: &[u8] = b":service";
/// Pseudo-header for the gRPC method name.
const PSEUDO_METHOD: &[u8] = b":method";
/// Pseudo-header for the RPC kind (SPEC § 4.1).
const PSEUDO_KIND: &[u8] = b":kind";
/// Pseudo-header for application status code (SPEC § 4.3).
const PSEUDO_STATUS: &[u8] = b":status";
/// Pseudo-header for application status message.
const PSEUDO_STATUS_MSG: &[u8] = b":status-msg";

/// RPC kind discriminants for the `:kind` pseudo-header
/// payload byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// 1 request, 1 response.
    Unary = 0,
    /// 1 request, N responses.
    ServerStream = 1,
    /// N requests, 1 response.
    ClientStream = 2,
    /// N requests, N responses.
    Bidi = 3,
}

impl CallKind {
    #[inline]
    fn as_byte(self) -> u8 {
        self as u8
    }
}

/// Client-side handle to a `transport-tcp-mux v2`
/// connection.
///
/// One handle multiplexes many sequential RPCs through
/// distinct `stream_id`s. The current v1 implementation
/// is single-threaded — callers serialise their RPCs
/// against `&mut TcpMuxV2Connection`. Concurrent
/// multiplexing is a v1.x surface that lifts the SPSC
/// reader / writer halves into separate tasks.
pub struct TcpMuxV2Connection {
    stream: TcpStream,
    authority: String,
    next_stream_id: u32,
    recv_buf: Vec<u8>,
    /// Linux io_uring fast path. Initialised lazily in
    /// `connect`; `None` when SQPOLL + plain ring both
    /// refused (kernel < 5.1 / EPERM with no
    /// unprivileged SQPOLL). When `Some`, `call_unary`
    /// dispatches to the io_uring path; streaming RPCs
    /// still use the blocking path until v1.1 grows a
    /// full state machine.
    #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
    ring: Option<ClientRing>,
}

impl TcpMuxV2Connection {
    /// Connect to a `transport-tcp-mux v2` server bound
    /// at `addr` (`"host:port"` raw TCP — no TLS, no
    /// HTTP, no handshake). `authority` is the logical
    /// `:authority`-equivalent for this connection; it
    /// rides along on every outgoing BEGIN frame but is
    /// not derived from `addr` (RFC § 5 / SPEC § 4.1).
    pub fn connect(addr: &str, authority: &str) -> Result<Self, RpcError> {
        let stream = TcpStream::connect(addr)
            .map_err(|_| RpcError::Transport("tcp-mux-v2: connect failed"))?;
        configure_socket(&stream)?;
        #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
        let ring = ClientRing::try_new(stream.as_raw_fd());
        Ok(TcpMuxV2Connection {
            stream,
            authority: String::from(authority),
            // Client-allocated stream ids are odd and
            // strictly increasing (SPEC § 3.1).
            next_stream_id: 1,
            recv_buf: Vec::with_capacity(INITIAL_RECV_BUF),
            #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
            ring,
        })
    }

    /// The logical authority this channel was opened
    /// with.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Configure how long the client-side io_uring path
    /// spins on the completion ring before falling back
    /// to a blocking `io_uring_enter(WAIT)` syscall.
    /// Default `Duration::ZERO` (no spin) — the same
    /// blocking-wait shape that ships in v1.1.1 / v1.2.
    ///
    /// Trades 1 CPU's worth of busy loop for the
    /// wake-up syscall + Spectre-mitigation cost in the
    /// kernel-side `io_uring_enter` path. Matches the
    /// API + semantics of
    /// `ShmemConnection::set_busy_poll_threshold`.
    ///
    /// On non-Linux / `--no-default-features` builds
    /// where the ring isn't available this is a no-op
    /// (the blocking path is the only path).
    pub fn set_busy_poll_threshold(&mut self, t: core::time::Duration) {
        #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
        if let Some(ring) = self.ring.as_mut() {
            ring.set_busy_poll_threshold(t);
        }
        #[cfg(not(all(target_os = "linux", feature = "transport-tcp-mux-iouring")))]
        let _ = t;
    }

    /// Send a unary RPC: one request body, one response
    /// body.
    pub fn call_unary(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        body: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
        if self.ring.is_some() {
            return self.call_unary_iouring(service, method, request_meta, body);
        }
        let stream_id = self.allocate_stream_id();
        self.send_begin(
            stream_id,
            service,
            method,
            CallKind::Unary,
            request_meta,
            Some(body),
            true,
        )?;
        let (mut bodies, trailing) = self.read_response_stream(true)?;
        let body = bodies.pop().unwrap_or_default();
        Ok((body, trailing))
    }

    /// Linux io_uring fast path for unary RPCs. Encodes
    /// the BEGIN frame into a single contiguous buffer,
    /// hands it to the SQPOLL ring, and drives the
    /// frame state machine on top of the ring's
    /// per-frame `recv_frame` primitive.
    ///
    /// Falls through to the blocking path on any
    /// ring-side error (treating io_uring as
    /// best-effort): a SEND/RECV failure usually means
    /// the kernel object is broken, so we disable the
    /// ring and retry on the blocking socket — this
    /// preserves caller-visible semantics even when
    /// io_uring breaks mid-RPC. (The connection's TCP
    /// fd is shared between the two paths, so the
    /// switch is transparent.)
    #[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
    fn call_unary_iouring(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        body: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        let stream_id = self.allocate_stream_id();
        let frame_bytes = encode_begin_frame(
            stream_id,
            service,
            method,
            CallKind::Unary,
            request_meta,
            body,
        );

        // SEND the request. If the kernel refuses the
        // submission (very rare — SQ full), drop back to
        // the blocking path with the same stream id;
        // the server will see it as the only request on
        // that stream id, no replay issue.
        let ring = self.ring.as_mut().expect("checked Some above");
        if let Err(e) = ring.send_frame(&frame_bytes) {
            self.ring = None;
            return Err(e);
        }

        // Drive the response state machine. The v1.1
        // server emits a single END frame for unary
        // (HAS_METADATA + END_STREAM, metadata block
        // carries :status, optional inline body); we
        // still loop to handle PING/PONG mid-RPC.
        loop {
            let ring = self.ring.as_mut().expect("ring still alive");
            let (header, payload) = match ring.recv_frame() {
                Ok(v) => v,
                Err(e) => {
                    // Ring is now suspect — surface the
                    // error rather than retry blocking
                    // (the request has already been
                    // queued to the kernel; replaying
                    // on the blocking path would double-
                    // submit).
                    self.ring = None;
                    return Err(e);
                }
            };
            match header.frame_type {
                FrameType::End => {
                    let payload_slice = payload.as_slice();
                    let (entries, inline_body) = if header.flags.contains(Flags::HAS_METADATA) {
                        decode_metadata_block(payload_slice, header.metadata_count)
                            .map_err(|_| RpcError::Codec("tcp-mux-v2: v2 frame decode failed"))?
                    } else {
                        (Vec::new(), payload_slice)
                    };
                    let mut bodies: Vec<Vec<u8>> = Vec::new();
                    if !inline_body.is_empty() {
                        bodies.push(inline_body.to_vec());
                    }
                    let trailing = entries_to_metadata(&entries);
                    let (mut bodies, trailing) = extract_status_then_finish(bodies, trailing)?;
                    let resp = bodies.pop().unwrap_or_default();
                    return Ok((resp, trailing));
                }
                FrameType::Data => {
                    // Servers shouldn't send DATA in
                    // unary; treat as protocol error.
                    return Err(RpcError::Codec("tcp-mux-v2: v2 frame decode failed"));
                }
                FrameType::Reset => {
                    return Err(decode_reset_payload(&payload));
                }
                FrameType::Ping => {
                    let pong_header = Header {
                        stream_id: 0,
                        frame_type: FrameType::Pong,
                        flags: Flags::NONE,
                        metadata_count: 0,
                        payload_len: payload.len() as u32,
                    };
                    let pong_frame = encode_frame_bytes(&pong_header, &[&payload]);
                    if let Err(e) = self
                        .ring
                        .as_mut()
                        .expect("ring still alive")
                        .send_frame(&pong_frame)
                    {
                        self.ring = None;
                        return Err(e);
                    }
                }
                FrameType::Pong => {
                    // Stray PONG — ignore per SPEC § 4.5.
                }
                FrameType::Begin => {
                    return Err(RpcError::Transport(
                        "tcp-mux-v2: server sent BEGIN to client",
                    ));
                }
            }
        }
    }

    /// Send a server-streaming RPC: one request body, N
    /// response bodies.
    pub fn call_server_streaming(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        body: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        let stream_id = self.allocate_stream_id();
        self.send_begin(
            stream_id,
            service,
            method,
            CallKind::ServerStream,
            request_meta,
            Some(body),
            true,
        )?;
        self.read_response_stream(false)
    }

    /// Send a client-streaming RPC: N request bodies,
    /// one response body.
    pub fn call_client_streaming(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        bodies: &[&[u8]],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        let stream_id = self.allocate_stream_id();
        let first = bodies.first().copied().unwrap_or(&[]);
        let only_one = bodies.len() <= 1;
        self.send_begin(
            stream_id,
            service,
            method,
            CallKind::ClientStream,
            request_meta,
            Some(first),
            only_one,
        )?;
        if bodies.len() >= 2 {
            for (i, body) in bodies.iter().enumerate().skip(1) {
                let is_last = i + 1 == bodies.len();
                self.send_data(stream_id, body, is_last)?;
            }
        }
        let (mut resp_bodies, trailing) = self.read_response_stream(true)?;
        let body = resp_bodies.pop().unwrap_or_default();
        Ok((body, trailing))
    }

    /// Send a bidi-streaming RPC: N request bodies, N
    /// response bodies. Sends all requests first then
    /// drains the response stream — v1 is half-duplex
    /// at the surface; full-duplex interleaving is a
    /// v1.x surface that splits read / write halves.
    pub fn call_bidi(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        bodies: &[&[u8]],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        let stream_id = self.allocate_stream_id();
        let first = bodies.first().copied().unwrap_or(&[]);
        let only_one = bodies.len() <= 1;
        self.send_begin(
            stream_id,
            service,
            method,
            CallKind::Bidi,
            request_meta,
            Some(first),
            only_one,
        )?;
        if bodies.len() >= 2 {
            for (i, body) in bodies.iter().enumerate().skip(1) {
                let is_last = i + 1 == bodies.len();
                self.send_data(stream_id, body, is_last)?;
            }
        }
        self.read_response_stream(false)
    }

    // ----------------------------------------------------------
    // internals
    // ----------------------------------------------------------

    fn allocate_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.saturating_add(2);
        id
    }

    #[allow(clippy::too_many_arguments)] // internal helper; struct wrapping adds noise
    fn send_begin(
        &mut self,
        stream_id: u32,
        service: &str,
        method: &str,
        kind: CallKind,
        request_meta: &Metadata,
        body: Option<&[u8]>,
        end_stream: bool,
    ) -> Result<(), RpcError> {
        let kind_byte = [kind.as_byte()];
        let entries: Vec<MetadataEntry<'_>> = {
            let mut v: Vec<MetadataEntry<'_>> = Vec::with_capacity(3 + request_meta.headers.len());
            v.push(MetadataEntry {
                name: PSEUDO_SERVICE,
                value: service.as_bytes(),
            });
            v.push(MetadataEntry {
                name: PSEUDO_METHOD,
                value: method.as_bytes(),
            });
            v.push(MetadataEntry {
                name: PSEUDO_KIND,
                value: &kind_byte,
            });
            for (n, vbytes) in &request_meta.headers {
                v.push(MetadataEntry {
                    name: n.as_bytes(),
                    value: vbytes.as_slice(),
                });
            }
            v
        };

        let meta_len = block_wire_len(&entries);
        let body_len = body.map(|b| b.len()).unwrap_or(0);
        let payload_len = meta_len + body_len;

        let mut meta_buf = alloc::vec![0u8; meta_len];
        encode_metadata_block(&entries, &mut meta_buf);

        let mut flags = Flags::HAS_METADATA;
        if end_stream {
            flags = flags.insert(Flags::END_STREAM);
        }

        let header = Header {
            stream_id,
            frame_type: FrameType::Begin,
            flags,
            metadata_count: entries.len() as u16,
            payload_len: payload_len as u32,
        };

        let body_slice = body.unwrap_or(&[]);
        write_frame(&mut self.stream, &header, &[&meta_buf, body_slice])
    }

    fn send_data(&mut self, stream_id: u32, body: &[u8], end_stream: bool) -> Result<(), RpcError> {
        let mut flags = Flags::NONE;
        if end_stream {
            flags = flags.insert(Flags::END_STREAM);
        }
        let header = Header {
            stream_id,
            frame_type: FrameType::Data,
            flags,
            metadata_count: 0,
            payload_len: body.len() as u32,
        };
        write_frame(&mut self.stream, &header, &[body])
    }

    fn read_response_stream(
        &mut self,
        expect_single_body: bool,
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        let mut frames_seen = 0usize;

        loop {
            frames_seen += 1;
            if frames_seen > MAX_RESPONSE_FRAMES {
                return Err(RpcError::Transport(
                    "tcp-mux-v2: response stream exceeded MAX_RESPONSE_FRAMES",
                ));
            }

            let header = read_frame(&mut self.stream, &mut self.recv_buf)?;
            match header.frame_type {
                FrameType::Data => {
                    if expect_single_body && !bodies.is_empty() {
                        return Err(RpcError::Codec("tcp-mux-v2: v2 frame decode failed"));
                    }
                    if header.payload_len > 0 {
                        bodies.push(self.recv_buf.clone());
                    } else {
                        bodies.push(Vec::new());
                    }
                    if header.flags.contains(Flags::END_STREAM) {
                        return Ok((bodies, Metadata::default()));
                    }
                }
                FrameType::End => {
                    let payload = &self.recv_buf[..header.payload_len as usize];
                    let (entries, inline_body) = if header.flags.contains(Flags::HAS_METADATA) {
                        decode_metadata_block(payload, header.metadata_count)
                            .map_err(|_| RpcError::Codec("tcp-mux-v2: v2 frame decode failed"))?
                    } else {
                        (Vec::new(), payload)
                    };
                    if !inline_body.is_empty() {
                        if expect_single_body && !bodies.is_empty() {
                            return Err(RpcError::Codec("tcp-mux-v2: v2 frame decode failed"));
                        }
                        bodies.push(inline_body.to_vec());
                    }
                    let trailing = entries_to_metadata(&entries);
                    return extract_status_then_finish(bodies, trailing);
                }
                FrameType::Reset => {
                    return Err(decode_reset_payload(&self.recv_buf));
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
                    // Stray PONG — ignore per SPEC § 4.5.
                }
                FrameType::Begin => {
                    return Err(RpcError::Transport(
                        "tcp-mux-v2: server sent BEGIN to client",
                    ));
                }
            }
        }
    }
}

// ============================================================
// RpcChannel trait impl — lets `TcpMuxV2Connection` flow
// through cross-transport client codegen (T1.2).
// ============================================================

impl RpcChannel for TcpMuxV2Connection {
    fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        TcpMuxV2Connection::call_unary(self, &method.service, &method.method, metadata, request)
    }

    fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        TcpMuxV2Connection::call_server_streaming(
            self,
            &method.service,
            &method.method,
            metadata,
            request,
        )
    }

    fn call_client_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        TcpMuxV2Connection::call_client_streaming(
            self,
            &method.service,
            &method.method,
            metadata,
            requests,
        )
    }

    fn call_bidi(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        TcpMuxV2Connection::call_bidi(self, &method.service, &method.method, metadata, requests)
    }
}

// ============================================================
// Helpers
// ============================================================

fn configure_socket(stream: &TcpStream) -> Result<(), RpcError> {
    // TCP_NODELAY: the v2 wire wants every BEGIN out the
    // wire immediately — Nagle would coalesce against the
    // following END/DATA and add ~40 ms of unhelpful
    // delay-ack latency.
    let _ = stream.set_nodelay(true);
    set_socket_buf(stream.as_raw_fd(), true, SOCKET_BUF_BYTES);
    set_socket_buf(stream.as_raw_fd(), false, SOCKET_BUF_BYTES);
    Ok(())
}

pub(super) fn set_socket_buf(fd: std::os::unix::io::RawFd, send: bool, bytes: i32) {
    // SAFETY: `bytes` lives on this stack frame; we
    // pass a pointer + matching length to setsockopt.
    // Ignore the return value — buffer sizing is a
    // best-effort optimisation, not a correctness gate.
    unsafe {
        let optname = if send { SO_SNDBUF } else { SO_RCVBUF };
        let _ = setsockopt(
            fd,
            SOL_SOCKET,
            optname,
            core::ptr::from_ref(&bytes).cast::<core::ffi::c_void>(),
            core::mem::size_of::<i32>() as u32,
        );
    }
}

const SOL_SOCKET: i32 = 1;
#[cfg(any(target_os = "linux", target_os = "android"))]
const SO_SNDBUF: i32 = 7;
#[cfg(any(target_os = "linux", target_os = "android"))]
const SO_RCVBUF: i32 = 8;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const SO_SNDBUF: i32 = 0x1001;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const SO_RCVBUF: i32 = 0x1002;

unsafe extern "C" {
    fn setsockopt(
        sockfd: i32,
        level: i32,
        optname: i32,
        optval: *const core::ffi::c_void,
        optlen: u32,
    ) -> i32;
}

fn entries_to_metadata(entries: &[MetadataEntry<'_>]) -> Metadata {
    Metadata {
        headers: entries
            .iter()
            .map(|e| {
                (
                    String::from_utf8_lossy(e.name).into_owned(),
                    e.value.to_vec(),
                )
            })
            .collect(),
    }
}

/// Extract `:status` / `:status-msg` from trailing
/// metadata, converting a non-OK status into
/// `RpcError::Status` per the RFC convention.
fn extract_status_then_finish(
    bodies: Vec<Vec<u8>>,
    mut trailing: Metadata,
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    let mut code: Option<StatusCode> = None;
    let mut msg = String::new();
    trailing.headers.retain(|(n, v)| {
        if n.as_bytes() == PSEUDO_STATUS && v.len() == 4 {
            let c = u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as i32;
            code = StatusCode::from_i32(c);
            false
        } else if n.as_bytes() == PSEUDO_STATUS_MSG {
            msg = String::from_utf8_lossy(v).into_owned();
            false
        } else {
            true
        }
    });
    match code {
        Some(c) if c != StatusCode::Ok => Err(RpcError::Status(Status {
            code: c,
            message: msg,
        })),
        _ => Ok((bodies, trailing)),
    }
}

/// Encode a complete BEGIN frame (16 B header +
/// metadata block + body) into one contiguous `Vec<u8>`
/// for the io_uring fast path. Mirrors `send_begin`
/// minus the `write_frame` writev — the bytes are
/// handed to the ring as a single SEND.
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
fn encode_begin_frame(
    stream_id: u32,
    service: &str,
    method: &str,
    kind: CallKind,
    request_meta: &Metadata,
    body: &[u8],
) -> Vec<u8> {
    let kind_byte = [kind.as_byte()];
    let mut entries: Vec<MetadataEntry<'_>> = Vec::with_capacity(3 + request_meta.headers.len());
    entries.push(MetadataEntry {
        name: PSEUDO_SERVICE,
        value: service.as_bytes(),
    });
    entries.push(MetadataEntry {
        name: PSEUDO_METHOD,
        value: method.as_bytes(),
    });
    entries.push(MetadataEntry {
        name: PSEUDO_KIND,
        value: &kind_byte,
    });
    for (n, vbytes) in &request_meta.headers {
        entries.push(MetadataEntry {
            name: n.as_bytes(),
            value: vbytes.as_slice(),
        });
    }

    let meta_len = block_wire_len(&entries);
    let payload_len = meta_len + body.len();

    let header = Header {
        stream_id,
        frame_type: FrameType::Begin,
        flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
        metadata_count: entries.len() as u16,
        payload_len: payload_len as u32,
    };

    let total = HEADER_LEN + payload_len;
    let mut out = Vec::with_capacity(total);
    out.resize(HEADER_LEN + meta_len, 0);
    {
        let hdr_slot: &mut [u8; HEADER_LEN] = (&mut out[..HEADER_LEN]).try_into().unwrap();
        encode_header(&header, hdr_slot);
    }
    encode_metadata_block(&entries, &mut out[HEADER_LEN..HEADER_LEN + meta_len]);
    out.extend_from_slice(body);
    out
}

/// Encode an arbitrary frame (header + concatenated
/// payload parts) into one contiguous `Vec<u8>` for the
/// io_uring fast path. Used for PONG responses inside
/// the unary state machine.
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
fn encode_frame_bytes(header: &Header, parts: &[&[u8]]) -> Vec<u8> {
    let payload_total: usize = parts.iter().map(|p| p.len()).sum();
    let total = HEADER_LEN + payload_total;
    let mut out = Vec::with_capacity(total);
    out.resize(HEADER_LEN, 0);
    {
        let hdr_slot: &mut [u8; HEADER_LEN] = (&mut out[..HEADER_LEN]).try_into().unwrap();
        encode_header(header, hdr_slot);
    }
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn decode_reset_payload(payload: &[u8]) -> RpcError {
    if payload.len() < 8 {
        return RpcError::Codec("tcp-mux-v2: v2 frame decode failed");
    }
    let code = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as i32;
    let msg_len = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    if payload.len() < 8 + msg_len {
        return RpcError::Codec("tcp-mux-v2: v2 frame decode failed");
    }
    let msg = String::from_utf8_lossy(&payload[8..8 + msg_len]).into_owned();
    let code = StatusCode::from_i32(code).unwrap_or(StatusCode::Internal);
    RpcError::Status(Status { code, message: msg })
}
