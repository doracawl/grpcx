//! Server-side `transport-uds` — `Server::listen_uds`
//! accept loop + per-connection thread that drains v2
//! frames, dispatches to registered `Service` impls
//! through [`Service::dispatch_with_ctx`], and writes
//! response frames back.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::connection::set_socket_buf;
use super::peer::fetch as fetch_peer_cred;
use super::wire::{read_frame, write_frame};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
use crate::{CallContext, Metadata, PeerCred, RpcError, Server, Status, StatusCode};

const PSEUDO_SERVICE: &[u8] = b":service";
const PSEUDO_METHOD: &[u8] = b":method";
const PSEUDO_KIND: &[u8] = b":kind";
const PSEUDO_STATUS: &[u8] = b":status";

/// Server-side accepted UDS connection. Created
/// internally by the `Server::listen_uds_*` accept loop;
/// not part of the public API today (constructor and
/// `serve` are crate-private). Reserved as a `pub`
/// surface so `accept_uds` can grow as a sans-IO API in
/// v1.x.
pub struct UdsServerConn {
    stream: UnixStream,
    peer_cred: Option<PeerCred>,
    recv_buf: Vec<u8>,
    streams: BTreeMap<u32, StreamState>,
}

struct StreamState {
    service: String,
    method: String,
    user_meta: Metadata,
    request_bodies: Vec<Vec<u8>>,
}

impl UdsServerConn {
    fn new(stream: UnixStream, peer_cred: Option<PeerCred>) -> Self {
        Self {
            stream,
            peer_cred,
            recv_buf: Vec::with_capacity(64 * 1024),
            streams: BTreeMap::new(),
        }
    }

    /// Kernel-attested credentials of the peer process,
    /// captured at accept time.
    pub fn peer_cred(&self) -> Option<&PeerCred> {
        self.peer_cred.as_ref()
    }

    fn serve(&mut self, server: &Server) -> Result<(), RpcError> {
        loop {
            let header = match read_frame(&mut self.stream, &mut self.recv_buf) {
                Ok(h) => h,
                // Peer closed mid-stream — exit cleanly.
                Err(_) => return Ok(()),
            };
            match header.frame_type {
                FrameType::Begin => self.handle_begin(server, header)?,
                FrameType::Data => self.handle_data(server, header)?,
                FrameType::End => {
                    // Bare END from client (no body) —
                    // closes a stream from the client
                    // side; dispatch with the accumulated
                    // requests.
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
                    // Client cancelled this stream — drop state.
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
                .map_err(|_| RpcError::Codec("uds: v2 metadata block decode failed"))?
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
                // SPEC § 4.1 — receivers MAY validate the
                // kind byte against the codegen schema.
                // v1 accepts any 0..=4; codegen-tied
                // validation is a T1.2 surface.
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
        ctx.set_peer_cred(self.peer_cred);

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
        // Fast path: no user trailing metadata. The whole
        // END metadata block is the pre-baked
        // `:status = OK` 17-byte literal — no Vec, no
        // `encode_metadata_block` call.
        if trailing.headers.is_empty() {
            let meta_bytes: &[u8] = &crate::wire_v2::STATUS_OK_BLOCK;
            let meta_count = crate::wire_v2::STATUS_OK_COUNT;
            let meta_len = meta_bytes.len();

            if bodies.len() <= 1 {
                let body = bodies.into_iter().next().unwrap_or_default();
                let header = Header {
                    stream_id,
                    frame_type: FrameType::End,
                    flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
                    metadata_count: meta_count,
                    payload_len: (meta_len + body.len()) as u32,
                };
                return write_frame(&mut self.stream, &header, &[meta_bytes, &body]);
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
                metadata_count: meta_count,
                payload_len: meta_len as u32,
            };
            return write_frame(&mut self.stream, &end_header, &[meta_bytes]);
        }

        // Slow path: build a generic block when the
        // handler attached user trailing metadata.
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

        // Single (or empty) response body → inline into
        // END per SPEC § 6.1 MUST.
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

        // Multi-body streaming response → N DATA + END(meta only).
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
    /// Bind to `path` and serve registered services over
    /// `transport-uds`. Blocks until the server is shut
    /// down (does not return — wrap in a thread).
    ///
    /// **Stale socket files are not auto-removed** —
    /// see RFC § 4.3. A residual file from a prior run
    /// causes `bind` to fail with
    /// `RpcError::Transport("uds: bind failed ...")`.
    pub fn listen_uds(&self, path: &Path) -> Result<(), RpcError> {
        let stop = AtomicBool::new(false);
        self.listen_uds_until(path, &stop)
    }

    /// As [`Self::listen_uds`], but exits cleanly when
    /// `stop` flips. Each accepted connection runs on
    /// its own OS thread and finishes when its peer
    /// closes the socket.
    pub fn listen_uds_until(&self, path: &Path, stop: &AtomicBool) -> Result<(), RpcError> {
        let listener = UnixListener::bind(path).map_err(|_| {
            RpcError::Transport(
                "uds: bind failed (address in use? — RFC § 4.3 documents the reclaim recipe)",
            )
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|_| RpcError::Transport("uds: listener set_nonblocking failed"))?;

        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // `accept` returns a blocking stream
                    // on Linux; ensure it on all
                    // platforms (kqueue may inherit the
                    // listener's nonblocking flag).
                    if let Err(e) = stream.set_nonblocking(false) {
                        // Best-effort; not a fatal error.
                        let _ = e;
                    }
                    let fd = stream.as_raw_fd();
                    set_socket_buf(fd, true, 16 * 1024 * 1024);
                    set_socket_buf(fd, false, 16 * 1024 * 1024);
                    let peer_cred = fetch_peer_cred(fd);
                    let server = self.clone();
                    let h = std::thread::spawn(move || {
                        let mut conn = UdsServerConn::new(stream, peer_cred);
                        let _ = conn.serve(&server);
                    });
                    handles.push(h);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(RpcError::Transport("uds: accept failed")),
            }
            handles.retain(|h| !h.is_finished());
        }
        Ok(())
    }
}
