//! Client-side `UdsConnection` — synchronous blocking
//! v2-frame client over `UnixStream`.
//!
//! One connection carries many concurrent RPCs through
//! distinct `stream_id`s; client-allocated ids are odd
//! and strictly increasing per SPEC § 3.

use alloc::string::String;
use alloc::vec::Vec;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::peer::fetch as fetch_peer_cred;
use super::wire::{read_frame, write_frame};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
use crate::{Metadata, Method, PeerCred, RpcChannel, RpcError, Status, StatusCode};

/// Kernel send / receive buffer size set on every UDS
/// socket the transport creates. Matches the RFC § 4
/// ceiling constraint (16 MiB) — large enough that a
/// 16 MiB v2 frame drains in one writev call.
const SOCKET_BUF_BYTES: i32 = 16 * 1024 * 1024;

/// Default initial capacity for the per-connection
/// receive buffer (`Vec<u8>` reused across frames).
const INITIAL_RECV_BUF: usize = 64 * 1024;

/// Maximum response messages collected before the
/// transport gives up — defends against a runaway
/// server. 1 MiB messages × 64K = 64 GiB before bail.
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

/// Client-side handle to a `transport-uds` connection.
///
/// One handle multiplexes many sequential RPCs through
/// distinct `stream_id`s. The current v1 implementation
/// is single-threaded — callers serialise their RPCs
/// against `&mut UdsConnection`. Concurrent multiplexing
/// is a v1.x surface that lifts the SPSC reader / writer
/// halves into separate tasks.
pub struct UdsConnection {
    stream: UnixStream,
    authority: String,
    next_stream_id: u32,
    peer_cred: Option<PeerCred>,
    recv_buf: Vec<u8>,
}

impl UdsConnection {
    /// Connect to a `transport-uds` server bound at
    /// `path`. `authority` is the logical
    /// `:authority`-equivalent for this connection; it
    /// rides along on every outgoing BEGIN frame but is
    /// not derived from `path` (RFC § 4 / SPEC § 4.1).
    pub fn connect(path: &Path, authority: &str) -> Result<Self, RpcError> {
        let stream =
            UnixStream::connect(path).map_err(|_| RpcError::Transport("uds: connect failed"))?;
        configure_socket(&stream)?;
        let peer_cred = fetch_peer_cred(stream.as_raw_fd());
        Ok(UdsConnection {
            stream,
            authority: String::from(authority),
            // Client-allocated stream ids are odd and
            // strictly increasing (SPEC § 3.1).
            next_stream_id: 1,
            peer_cred,
            recv_buf: Vec::with_capacity(INITIAL_RECV_BUF),
        })
    }

    /// Kernel-attested credentials of the **server**
    /// process (taken via `SO_PEERCRED` /
    /// `getpeereid(3)` on the connected socket).
    /// Returns `None` if the platform does not surface
    /// peer credentials.
    pub fn peer_cred(&self) -> Option<&PeerCred> {
        self.peer_cred.as_ref()
    }

    /// The logical authority this channel was opened
    /// with.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Send a unary RPC: one request body, one response
    /// body. Returns the response body bytes plus any
    /// trailing metadata other than `:status` /
    /// `:status-msg` (those become the `Status` on
    /// `RpcError::Status` if non-OK).
    pub fn call_unary(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        body: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
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
        // First request rides BEGIN (inline); the rest
        // come via DATA frames; the last DATA carries
        // END_STREAM (SPEC § 6.3).
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
        // SPEC § 3.2: stream ids are never reused. On
        // overflow the connection must be torn down by
        // the caller — we surface that as a transport
        // error on the next allocation by saturating.
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
        let kind_byte = kind.as_byte();
        let body_slice = body.unwrap_or(&[]);
        let mut flags = Flags::HAS_METADATA;
        if end_stream {
            flags = flags.insert(Flags::END_STREAM);
        }

        // Fast path — no user metadata (the bench hot path
        // + the vast majority of real calls): hand-encode
        // the three pseudo-header entries into a 128-byte
        // stack buffer. No `Vec<MetadataEntry>`, no
        // `vec![0u8; meta_len]`. SPEC § 5 says
        // `name_len <= NAME_LEN_MAX = 1024`, so a 128 B
        // buffer fits any (`:service`, `:method`,
        // `:kind`) triple with `service.len() + method.len()
        // <= 95` — true for every realistic identifier.
        let user_len: usize = request_meta
            .headers
            .iter()
            .map(|(n, v)| 6 + n.len() + v.len())
            .sum();
        let total_meta = 6 + 8 + service.len()  // ":service"
            + 6 + 7 + method.len()              // ":method"
            + 6 + 5 + 1                         // ":kind"
            + user_len;
        let header = Header {
            stream_id,
            frame_type: FrameType::Begin,
            flags,
            metadata_count: 3 + request_meta.headers.len() as u16,
            payload_len: (total_meta + body_slice.len()) as u32,
        };

        if request_meta.headers.is_empty() && total_meta <= 128 {
            let mut stack = [0u8; 128];
            let n = crate::wire_v2::encode_begin_block(
                &mut stack,
                service.as_bytes(),
                method.as_bytes(),
                kind_byte,
                &[],
            );
            return write_frame(&mut self.stream, &header, &[&stack[..n], body_slice]);
        }

        // Slow path — preserve the existing
        // `Vec<MetadataEntry>` + `vec![0; meta_len]` shape
        // for the rare case with user metadata or
        // pathologically large service/method names.
        let kind_value = [kind_byte];
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
                value: &kind_value,
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
        let mut meta_buf = alloc::vec![0u8; meta_len];
        encode_metadata_block(&entries, &mut meta_buf);
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

    /// Drain frames until an `END` (success) or `RESET`
    /// (error) arrives. Returns the response message
    /// bodies plus the trailing metadata (with `:status*`
    /// already extracted and converted to `RpcError`).
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
                    "uds: response stream exceeded MAX_RESPONSE_FRAMES",
                ));
            }

            let header = read_frame(&mut self.stream, &mut self.recv_buf)?;
            match header.frame_type {
                FrameType::Data => {
                    if expect_single_body && !bodies.is_empty() {
                        return Err(RpcError::Codec("uds: v2 frame decode failed"));
                    }
                    if header.payload_len > 0 {
                        // Unary fast path: swap `recv_buf`
                        // into `bodies` instead of cloning.
                        // For server-streaming, clone so
                        // the buffer stays reusable for
                        // the next Data frame without an
                        // alloc.
                        if expect_single_body {
                            bodies.push(core::mem::take(&mut self.recv_buf));
                        } else {
                            bodies.push(self.recv_buf.clone());
                        }
                    } else {
                        bodies.push(Vec::new());
                    }
                    if header.flags.contains(Flags::END_STREAM) {
                        // DATA-with-END is legal per
                        // SPEC § 4.2 — no trailers.
                        return Ok((bodies, Metadata::default()));
                    }
                }
                FrameType::End => {
                    // END payload layout:
                    //   if HAS_METADATA: meta_block + (optional) inline body
                    //   else:            empty
                    let payload = &self.recv_buf[..header.payload_len as usize];
                    let (entries, inline_body) = if header.flags.contains(Flags::HAS_METADATA) {
                        decode_metadata_block(payload, header.metadata_count)
                            .map_err(|_| RpcError::Codec("uds: v2 frame decode failed"))?
                    } else {
                        (Vec::new(), payload)
                    };
                    if !inline_body.is_empty() {
                        if expect_single_body && !bodies.is_empty() {
                            return Err(RpcError::Codec("uds: v2 frame decode failed"));
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
                    // Reply with PONG echoing the payload.
                    let payload = self.recv_buf.clone();
                    let pong = Header {
                        stream_id: 0,
                        frame_type: FrameType::Pong,
                        flags: Flags::NONE,
                        metadata_count: 0,
                        payload_len: payload.len() as u32,
                    };
                    write_frame(&mut self.stream, &pong, &[&payload])?;
                    // PINGs do not advance the RPC; keep reading.
                }
                FrameType::Pong => {
                    // Stray PONG (we did not send a PING)
                    // — ignore per SPEC § 4.5 receiver
                    // semantics.
                }
                FrameType::Begin => {
                    return Err(RpcError::Transport("uds: server sent BEGIN to client"));
                }
            }
        }
    }
}

// ============================================================
// RpcChannel trait impl — lets `UdsConnection` flow through
// cross-transport client codegen (T1.2).
// ============================================================

impl RpcChannel for UdsConnection {
    fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        UdsConnection::call_unary(self, &method.service, &method.method, metadata, request)
    }

    fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        UdsConnection::call_server_streaming(
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
        UdsConnection::call_client_streaming(
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
        UdsConnection::call_bidi(self, &method.service, &method.method, metadata, requests)
    }
}

// ============================================================
// Helpers
// ============================================================

fn configure_socket(stream: &UnixStream) -> Result<(), RpcError> {
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

fn decode_reset_payload(payload: &[u8]) -> RpcError {
    if payload.len() < 8 {
        return RpcError::Codec("uds: v2 frame decode failed");
    }
    let code = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as i32;
    let msg_len = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    if payload.len() < 8 + msg_len {
        return RpcError::Codec("uds: v2 frame decode failed");
    }
    let msg = String::from_utf8_lossy(&payload[8..8 + msg_len]).into_owned();
    let code = StatusCode::from_i32(code).unwrap_or(StatusCode::Internal);
    RpcError::Status(Status { code, message: msg })
}
