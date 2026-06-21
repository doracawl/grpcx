//! Client-side `ShmemConnection` — synchronous blocking
//! v2-frame client over a shared-memory ring pair.
//!
//! Mirrors the structure of [`crate::UdsConnection`] but
//! replaces UDS read/write with ring push/pop + wake-fd
//! signalling. Same dispatch helpers (BEGIN inline body,
//! END inline body, RESET status decode, etc.).

use alloc::string::String;
use alloc::vec::Vec;
use std::path::Path;
use std::time::{Duration, Instant};

use super::rendezvous::{RegionDescriptor, connect_rendezvous};
use super::ring::{self, Ring, RingError, SLOT_STRIDE};
use super::wake::{wake_signal, wake_wait};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
use crate::{Metadata, Method, PeerCred, RpcChannel, RpcError, Status, StatusCode};

/// Default initial capacity for the per-connection
/// receive buffer.
const INITIAL_RECV_BUF: usize = SLOT_STRIDE;

/// Pseudo-headers (SPEC § 4.1 / § 4.3).
const PSEUDO_SERVICE: &[u8] = b":service";
const PSEUDO_METHOD: &[u8] = b":method";
const PSEUDO_KIND: &[u8] = b":kind";
const PSEUDO_STATUS: &[u8] = b":status";
const PSEUDO_STATUS_MSG: &[u8] = b":status-msg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    Unary = 0,
    ServerStream = 1,
    ClientStream = 2,
    Bidi = 3,
}

/// Client handle for a `transport-shmem` connection.
///
/// The handle owns: the mmap'd [`super::region::Region`],
/// the two wake fds it received via SCM_RIGHTS, and a
/// pair of [`Ring`]s carved out of the region. RPCs run
/// synchronously on `&mut self`; concurrent multiplexing
/// is a v1.x surface.
pub struct ShmemConnection {
    // Owned through rendezvous output — kept alive so the
    // mmap stays mapped for the connection's lifetime.
    _region: super::region::Region,
    send: Ring,
    recv: Ring,
    wait_file: std::fs::File,
    signal_file: std::fs::File,
    authority: String,
    next_stream_id: u32,
    busy_poll_us: u64,
    busy_poll_unbounded: bool,
    recv_buf: Vec<u8>,
    // Rigtorp cached opposite-side positions. Producer-
    // local cached reader_pos for `send`; consumer-local
    // cached writer_pos for `recv`. Both initialised to 0
    // (matches post-reset state).
    send_cached_rpos: u64,
    recv_cached_wpos: u64,
}

impl ShmemConnection {
    /// Open a shmem connection by performing a rendezvous
    /// handshake against the AF_UNIX socket at
    /// `rendezvous_path`.
    pub fn connect(rendezvous_path: &Path, authority: &str) -> Result<Self, RpcError> {
        let RegionDescriptor {
            region,
            slot_count: _,
            slot_stride: _,
            wait_fd,
            signal_fd,
        } = connect_rendezvous(rendezvous_path)?;
        Self::from_descriptor(region, wait_fd, signal_fd, authority)
    }

    fn from_descriptor(
        region: super::region::Region,
        wait_fd: std::os::fd::OwnedFd,
        signal_fd: std::os::fd::OwnedFd,
        authority: &str,
    ) -> Result<Self, RpcError> {
        // SAFETY: rendezvous gave us a region shared with
        // the server; we hold both Rings for our role.
        let rings = unsafe { ring::client_rings(&region) };
        Ok(ShmemConnection {
            _region: region,
            send: rings.send,
            recv: rings.recv,
            wait_file: std::fs::File::from(wait_fd),
            signal_file: std::fs::File::from(signal_fd),
            authority: String::from(authority),
            next_stream_id: 1,
            busy_poll_us: 0,
            busy_poll_unbounded: false,
            recv_buf: Vec::with_capacity(INITIAL_RECV_BUF),
            send_cached_rpos: 0,
            recv_cached_wpos: 0,
        })
    }

    /// The logical authority this channel was opened
    /// with. Mirrors [`crate::UdsConnection::authority`].
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Kernel-attested peer credentials. T3.1 does not
    /// surface peer cred yet for shmem; placeholder
    /// returns `None`. Wiring lives in T3.2.x.
    pub fn peer_cred(&self) -> Option<&PeerCred> {
        None
    }

    /// Configure how long the consumer side spins on a
    /// ring head before falling back to blocking on the
    /// wake fd. Default `Duration::ZERO` — no spin.
    pub fn set_busy_poll_threshold(&mut self, t: Duration) {
        self.busy_poll_us = t.as_micros() as u64;
    }

    /// Enter / leave unbounded busy-poll mode. When set,
    /// both ends skip wake-fd signalling — the consumer
    /// spins forever on the ring head, and the producer
    /// publishes without poking the wake pipe. This
    /// eliminates the per-frame syscall that dominates
    /// high-throughput pushes, at the cost of one CPU per
    /// active connection on each side.
    ///
    /// Both endpoints must agree: the peer worker must
    /// also be in unbounded mode (`Server::listen_shmem_
    /// busy_poll_until`). If only one side opts in, the
    /// other will fill its wake pipe (writer blocks) or
    /// read empty bytes (reader misses publishes).
    pub fn set_busy_poll_unbounded(&mut self, on: bool) {
        self.busy_poll_unbounded = on;
    }

    /// Unary RPC.
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
        Ok((bodies.pop().unwrap_or_default(), trailing))
    }

    /// Server-streaming RPC.
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

    /// **Zero-copy server-streaming RPC.** Each Data frame's
    /// payload is delivered to `on_data` as a `&[u8]` borrow
    /// directly into the shared-memory ring slot — no
    /// per-frame `memcpy` out of shmem, no per-frame heap
    /// allocation. The borrow is valid only during the
    /// callback; the slot is released as soon as `on_data`
    /// returns and may be overwritten by the next push.
    ///
    /// Returns the trailing metadata once the End frame
    /// arrives. Bench-grade hot path; standard
    /// `call_server_streaming` (owned `Vec<Vec<u8>>`)
    /// remains available for typical callers.
    pub fn call_server_streaming_view<F>(
        &mut self,
        service: &str,
        method: &str,
        request_meta: &Metadata,
        body: &[u8],
        mut on_data: F,
    ) -> Result<Metadata, RpcError>
    where
        F: FnMut(&[u8]),
    {
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
        // Hot loop: zero-copy pop, callback, advance.
        // Unbounded busy-poll mode mirrors `pop_blocking`.
        loop {
            let (header, advance) = self.recv_view_one(&mut on_data)?;
            if advance {
                self.recv.advance_reader();
            }
            match header {
                Some(h) if h.frame_type == FrameType::End => {
                    return self.recv_end_trailing(h);
                }
                Some(h) if h.frame_type == FrameType::Reset => {
                    return Err(decode_reset_payload(&self.recv_buf));
                }
                _ => {}
            }
        }
    }

    /// Inner spin loop: wait for one frame on the recv
    /// ring (busy-poll, no wake-fd in unbounded mode);
    /// for Data, invoke `on_data` with the zero-copy
    /// payload view and advance the reader. For End /
    /// Reset, return the header (caller handles trailing).
    /// Returns `(Some(Header), advance_done)` for terminal
    /// frames or per-Data; `(None, false)` is impossible.
    fn recv_view_one<F>(&mut self, on_data: &mut F) -> Result<(Option<Header>, bool), RpcError>
    where
        F: FnMut(&[u8]),
    {
        loop {
            let result = self.recv.pop_view_cached(&mut self.recv_cached_wpos);
            match result {
                Ok((header, payload)) => match header.frame_type {
                    FrameType::Data => {
                        on_data(payload);
                        // Advance now, before returning,
                        // so the borrow is released.
                        self.recv.advance_reader();
                        // Don't double-advance in caller.
                        return Ok((Some(header), false));
                    }
                    FrameType::End => {
                        // Copy End payload out of the slot
                        // because we need to decode metadata
                        // AFTER releasing the borrow (the
                        // caller might call more methods on
                        // self that touch recv_buf).
                        let plen = header.payload_len as usize;
                        self.recv_buf.clear();
                        self.recv_buf.extend_from_slice(payload);
                        debug_assert_eq!(self.recv_buf.len(), plen);
                        return Ok((Some(header), true));
                    }
                    FrameType::Reset => {
                        let plen = header.payload_len as usize;
                        self.recv_buf.clear();
                        self.recv_buf.extend_from_slice(payload);
                        debug_assert_eq!(self.recv_buf.len(), plen);
                        return Ok((Some(header), true));
                    }
                    _ => return Err(RpcError::Transport("shmem: unexpected frame from server")),
                },
                Err(RingError::Empty) => {
                    if self.busy_poll_unbounded {
                        core::hint::spin_loop();
                    } else if self.busy_poll_us > 0 {
                        // Bounded busy-poll: fall back to
                        // wait_file if window expired.
                        let _ = wake_wait(&mut self.wait_file);
                    } else {
                        let _ = wake_wait(&mut self.wait_file);
                    }
                }
                Err(_) => return Err(RpcError::Transport("shmem: ring pop error")),
            }
        }
    }

    fn recv_end_trailing(&mut self, header: Header) -> Result<Metadata, RpcError> {
        let payload = &self.recv_buf[..header.payload_len as usize];
        let (entries, _inline_body) = if header.flags.contains(Flags::HAS_METADATA) {
            decode_metadata_block(payload, header.metadata_count)
                .map_err(|_| RpcError::Codec("shmem: bad END metadata"))?
        } else {
            (Vec::new(), payload)
        };
        // Inline body on End is unusual on server-streaming;
        // we ignore it for the zero-copy callback path.
        let trailing = entries_to_metadata(&entries);
        let (_bodies, trailing) = extract_status_then_finish(Vec::new(), trailing)?;
        Ok(trailing)
    }

    /// Client-streaming RPC.
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
            for (i, b) in bodies.iter().enumerate().skip(1) {
                self.send_data(stream_id, b, i + 1 == bodies.len())?;
            }
        }
        let (mut resp, trailing) = self.read_response_stream(true)?;
        Ok((resp.pop().unwrap_or_default(), trailing))
    }

    /// Bidirectional-streaming RPC (half-duplex v1).
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
            for (i, b) in bodies.iter().enumerate().skip(1) {
                self.send_data(stream_id, b, i + 1 == bodies.len())?;
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

    #[allow(clippy::too_many_arguments)] // internal helper
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
        let kind_byte = [kind as u8];
        let mut entries: Vec<MetadataEntry<'_>> =
            Vec::with_capacity(3 + request_meta.headers.len());
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
        for (n, v) in &request_meta.headers {
            entries.push(MetadataEntry {
                name: n.as_bytes(),
                value: v.as_slice(),
            });
        }

        let meta_len = block_wire_len(&entries);
        let body_len = body.map(|b| b.len()).unwrap_or(0);
        let payload_len = meta_len + body_len;

        let mut payload = alloc::vec![0u8; payload_len];
        let written = encode_metadata_block(&entries, &mut payload[..meta_len]);
        debug_assert_eq!(written, meta_len);
        if let Some(b) = body {
            payload[meta_len..meta_len + b.len()].copy_from_slice(b);
        }

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
        self.push_with_wake(&header, &payload)
    }

    fn send_data(&mut self, stream_id: u32, body: &[u8], end_stream: bool) -> Result<(), RpcError> {
        let header = Header {
            stream_id,
            frame_type: FrameType::Data,
            flags: if end_stream {
                Flags::END_STREAM
            } else {
                Flags::NONE
            },
            metadata_count: 0,
            payload_len: body.len() as u32,
        };
        self.push_with_wake(&header, body)
    }

    fn push_with_wake(&mut self, header: &Header, payload: &[u8]) -> Result<(), RpcError> {
        if self.busy_poll_unbounded {
            loop {
                match self
                    .send
                    .push_cached(header, payload, &mut self.send_cached_rpos)
                {
                    Ok(()) => return Ok(()),
                    Err(RingError::Full) => core::hint::spin_loop(),
                    Err(RingError::FrameTooLarge) => {
                        return Err(RpcError::Transport("shmem: frame too large for slot"));
                    }
                    Err(_) => return Err(RpcError::Transport("shmem: ring push error")),
                }
            }
        }
        loop {
            match self
                .send
                .push_cached(header, payload, &mut self.send_cached_rpos)
            {
                Ok(()) => {
                    let _ = wake_signal(&mut self.signal_file);
                    return Ok(());
                }
                Err(RingError::Full) => {
                    let _ = wake_signal(&mut self.signal_file);
                    std::thread::sleep(Duration::from_micros(10));
                }
                Err(RingError::FrameTooLarge) => {
                    return Err(RpcError::Transport("shmem: frame too large for slot"));
                }
                Err(_) => return Err(RpcError::Transport("shmem: ring push error")),
            }
        }
    }

    fn pop_blocking(&mut self) -> Result<Header, RpcError> {
        if self.busy_poll_unbounded {
            loop {
                match self
                    .recv
                    .pop_cached(&mut self.recv_buf, &mut self.recv_cached_wpos)
                {
                    Ok(h) => return Ok(h),
                    Err(RingError::Empty) => core::hint::spin_loop(),
                    Err(_) => return Err(RpcError::Transport("shmem: ring pop error")),
                }
            }
        }
        if self.busy_poll_us > 0 {
            let deadline = Instant::now() + Duration::from_micros(self.busy_poll_us);
            loop {
                match self
                    .recv
                    .pop_cached(&mut self.recv_buf, &mut self.recv_cached_wpos)
                {
                    Ok(h) => return Ok(h),
                    Err(RingError::Empty) => {
                        if Instant::now() >= deadline {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                    Err(_) => return Err(RpcError::Transport("shmem: ring pop error")),
                }
            }
        }
        loop {
            match self
                .recv
                .pop_cached(&mut self.recv_buf, &mut self.recv_cached_wpos)
            {
                Ok(h) => return Ok(h),
                Err(RingError::Empty) => {
                    let _ = wake_wait(&mut self.wait_file);
                }
                Err(_) => return Err(RpcError::Transport("shmem: ring pop error")),
            }
        }
    }

    fn read_response_stream(
        &mut self,
        expect_single: bool,
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        loop {
            let header = self.pop_blocking()?;
            match header.frame_type {
                FrameType::Data => {
                    if expect_single && !bodies.is_empty() {
                        return Err(RpcError::Codec("shmem: extra DATA for unary"));
                    }
                    // Both unary and server-streaming take
                    // the swap path now: `mem::take` the
                    // recv_buf into bodies, then re-allocate
                    // a fresh recv_buf for the next pop.
                    // Trades the per-frame 3 KiB clone for
                    // a per-frame heap alloc — net win
                    // because malloc fast-path is ~40 ns
                    // and the avoided memcpy is ~200 ns on
                    // 3 KiB payloads. The next pop()
                    // resize() will reuse the new alloc.
                    bodies.push(core::mem::take(&mut self.recv_buf));
                    self.recv_buf = Vec::with_capacity(INITIAL_RECV_BUF);
                    if header.flags.contains(Flags::END_STREAM) {
                        return Ok((bodies, Metadata::default()));
                    }
                }
                FrameType::End => {
                    let payload = &self.recv_buf[..header.payload_len as usize];
                    let (entries, inline_body) = if header.flags.contains(Flags::HAS_METADATA) {
                        decode_metadata_block(payload, header.metadata_count)
                            .map_err(|_| RpcError::Codec("shmem: bad END metadata"))?
                    } else {
                        (Vec::new(), payload)
                    };
                    if !inline_body.is_empty() {
                        if expect_single && !bodies.is_empty() {
                            return Err(RpcError::Codec("shmem: extra body on END"));
                        }
                        bodies.push(inline_body.to_vec());
                    }
                    let trailing = entries_to_metadata(&entries);
                    return extract_status_then_finish(bodies, trailing);
                }
                FrameType::Reset => {
                    return Err(decode_reset_payload(&self.recv_buf));
                }
                _ => return Err(RpcError::Transport("shmem: unexpected frame from server")),
            }
        }
    }
}

// ============================================================
// helpers (mirror of transport_uds/connection.rs)
// ============================================================

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
        return RpcError::Codec("shmem: short RESET payload");
    }
    let code = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as i32;
    let msg_len = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    if payload.len() < 8 + msg_len {
        return RpcError::Codec("shmem: truncated RESET");
    }
    let msg = String::from_utf8_lossy(&payload[8..8 + msg_len]).into_owned();
    let code = StatusCode::from_i32(code).unwrap_or(StatusCode::Internal);
    RpcError::Status(Status { code, message: msg })
}

// ============================================================
// RpcChannel impl — lets `ShmemConnection` flow through
// cross-transport client codegen.
// ============================================================

impl RpcChannel for ShmemConnection {
    fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        ShmemConnection::call_unary(self, &method.service, &method.method, metadata, request)
    }
    fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        ShmemConnection::call_server_streaming(
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
        ShmemConnection::call_client_streaming(
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
        ShmemConnection::call_bidi(self, &method.service, &method.method, metadata, requests)
    }
}
