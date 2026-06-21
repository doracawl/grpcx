//! Server-side `transport-shmem` — `Server::listen_shmem`
//! accept loop. Each accepted UDS rendezvous connection
//! gets:
//! - a region + wake fds (via `accept_rendezvous`)
//! - a worker OS thread that drains the c2s ring, dispatches
//!   into registered `Service`s through
//!   `Service::dispatch_with_ctx`, and publishes responses
//!   onto the s2c ring.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::rendezvous::{ServerSideRegion, accept_rendezvous};
use super::ring::{self, Ring, RingError};
use crate::wire_v2::{
    Flags, FrameType, Header, MetadataEntry, block_wire_len, decode_metadata_block,
    encode_metadata_block,
};
use crate::{CallContext, Metadata, RpcError, Server, StatusCode};

// Region sizing handed to `accept_rendezvous`. The
// product `SLOT_COUNT * SLOT_STRIDE` is the total
// mmap'd region — large enough to fit
// `MIN_REGION_BYTES = 256 + 2 * SLOTS_PER_RING (=16) *
// SLOT_STRIDE (=4096) = 131_328` (≈ 128 KiB).
const SLOT_COUNT: u32 = 64;
const SLOT_STRIDE: u32 = 4096;

const PSEUDO_SERVICE: &[u8] = b":service";
const PSEUDO_METHOD: &[u8] = b":method";
const PSEUDO_KIND: &[u8] = b":kind";
const PSEUDO_STATUS: &[u8] = b":status";

// ============================================================
// Per-connection worker
// ============================================================

struct StreamState {
    service: String,
    method: String,
    user_meta: Metadata,
    request_bodies: Vec<Vec<u8>>,
}

struct ShmemConnWorker {
    recv: Ring,
    send: Ring,
    wait_file: std::fs::File,
    signal_file: std::fs::File,
    recv_buf: Vec<u8>,
    streams: BTreeMap<u32, StreamState>,
    busy_poll_unbounded: bool,
    // Rigtorp cached opposite-side positions.
    recv_cached_wpos: u64,
    send_cached_rpos: u64,
    _region: super::region::Region,
}

impl ShmemConnWorker {
    fn new(side: ServerSideRegion, busy_poll_unbounded: bool) -> Self {
        // SAFETY: side.region is shared with the client;
        // we hold the consumer end of c2s and producer
        // end of s2c. POSIX shm guarantees zero-filled
        // pages on `ftruncate`-grow + `mmap` — both
        // writer_pos and reader_pos start at 0.
        let rings = unsafe { ring::server_rings(&side.region) };
        ShmemConnWorker {
            recv: rings.recv,
            send: rings.send,
            wait_file: std::fs::File::from(side.wait_fd),
            signal_file: std::fs::File::from(side.signal_fd),
            recv_buf: Vec::with_capacity(super::ring::SLOT_STRIDE),
            streams: BTreeMap::new(),
            busy_poll_unbounded,
            recv_cached_wpos: 0,
            send_cached_rpos: 0,
            _region: side.region,
        }
    }

    fn serve(&mut self, server: &Server) {
        loop {
            let header = match self.pop_blocking() {
                Ok(h) => h,
                Err(_) => return,
            };
            if self.handle_frame(server, header).is_err() {
                return;
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
        loop {
            match self
                .recv
                .pop_cached(&mut self.recv_buf, &mut self.recv_cached_wpos)
            {
                Ok(h) => return Ok(h),
                Err(RingError::Empty) => {
                    let mut byte = [0u8; 1];
                    if self.wait_file.read(&mut byte).is_err() {
                        return Err(RpcError::Transport("shmem: wait fd closed"));
                    }
                }
                Err(_) => return Err(RpcError::Transport("shmem: ring pop error")),
            }
        }
    }

    fn handle_frame(&mut self, server: &Server, header: Header) -> Result<(), RpcError> {
        match header.frame_type {
            FrameType::Begin => self.handle_begin(server, header),
            FrameType::Data => self.handle_data(server, header),
            FrameType::End => {
                if let Some(st) = self.streams.remove(&header.stream_id) {
                    self.dispatch_stream(server, header.stream_id, st)
                } else {
                    Ok(())
                }
            }
            FrameType::Reset => {
                self.streams.remove(&header.stream_id);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn handle_begin(&mut self, server: &Server, header: Header) -> Result<(), RpcError> {
        let payload_len = header.payload_len as usize;
        let payload = &self.recv_buf[..payload_len];
        let (entries, body) = if header.flags.contains(Flags::HAS_METADATA) {
            decode_metadata_block(payload, header.metadata_count)
                .map_err(|_| RpcError::Codec("shmem: bad BEGIN metadata"))?
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
                // ignore — codegen validates client-side
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
        let plen = header.payload_len as usize;
        let body = self.recv_buf[..plen].to_vec();
        let dispatch_now = if let Some(st) = self.streams.get_mut(&header.stream_id) {
            if !body.is_empty() {
                st.request_bodies.push(body);
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
        match svc.dispatch_with_ctx(&st.method, &mut ctx, &st.request_bodies) {
            Ok(resps) => {
                let trailing = ctx.into_trailing();
                self.send_response(stream_id, resps, trailing)
            }
            Err(RpcError::Status(s)) => self.send_reset(stream_id, s.code, &s.message),
            Err(_) => self.send_reset(stream_id, StatusCode::Internal, "handler error"),
        }
    }

    fn send_response(
        &mut self,
        stream_id: u32,
        bodies: Vec<Vec<u8>>,
        trailing: Metadata,
    ) -> Result<(), RpcError> {
        // Hot-path elision: handlers can stamp
        // `x-broadcast-body = u32 LE N` on the trailing
        // metadata to request that the single body in
        // `bodies` be pushed N times as separate Data
        // frames, sharing one allocation. Used by the
        // bench `bulk` handler to avoid materialising
        // N×3 KiB body clones server-side.
        let mut broadcast_count: Option<u32> = None;
        let mut filtered_trailing: Vec<(&str, &[u8])> = Vec::with_capacity(trailing.headers.len());
        for (n, v) in &trailing.headers {
            if n == "x-broadcast-body" && v.len() == 4 {
                broadcast_count = Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]));
            } else {
                filtered_trailing.push((n.as_str(), v.as_slice()));
            }
        }

        let status_ok = [0u8; 4];
        let mut trailing_entries: Vec<MetadataEntry<'_>> =
            Vec::with_capacity(1 + filtered_trailing.len());
        trailing_entries.push(MetadataEntry {
            name: PSEUDO_STATUS,
            value: &status_ok,
        });
        for (n, v) in &filtered_trailing {
            trailing_entries.push(MetadataEntry {
                name: n.as_bytes(),
                value: v,
            });
        }
        let meta_len = block_wire_len(&trailing_entries);
        let mut meta_buf = alloc::vec![0u8; meta_len];
        encode_metadata_block(&trailing_entries, &mut meta_buf);

        // Broadcast path: one body, push N copies.
        if let Some(n) = broadcast_count
            && bodies.len() == 1
        {
            let body = bodies.into_iter().next().unwrap_or_default();
            let header_template = Header {
                stream_id,
                frame_type: FrameType::Data,
                flags: Flags::NONE,
                metadata_count: 0,
                payload_len: body.len() as u32,
            };
            for _ in 0..n {
                self.push_with_wake(&header_template, &body)?;
            }
            let end_header = Header {
                stream_id,
                frame_type: FrameType::End,
                flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
                metadata_count: trailing_entries.len() as u16,
                payload_len: meta_len as u32,
            };
            return self.push_with_wake(&end_header, &meta_buf);
        }

        if bodies.len() <= 1 {
            let body = bodies.into_iter().next().unwrap_or_default();
            let payload_len = meta_len + body.len();
            if payload_len + crate::wire_v2::HEADER_LEN > super::ring::SLOT_STRIDE {
                return self.send_reset(
                    stream_id,
                    StatusCode::ResourceExhausted,
                    "response exceeds shmem slot",
                );
            }
            let mut payload = alloc::vec![0u8; payload_len];
            payload[..meta_len].copy_from_slice(&meta_buf);
            payload[meta_len..].copy_from_slice(&body);
            let header = Header {
                stream_id,
                frame_type: FrameType::End,
                flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
                metadata_count: trailing_entries.len() as u16,
                payload_len: payload_len as u32,
            };
            return self.push_with_wake(&header, &payload);
        }

        for body in &bodies {
            let header = Header {
                stream_id,
                frame_type: FrameType::Data,
                flags: Flags::NONE,
                metadata_count: 0,
                payload_len: body.len() as u32,
            };
            self.push_with_wake(&header, body)?;
        }
        let end_header = Header {
            stream_id,
            frame_type: FrameType::End,
            flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
            metadata_count: trailing_entries.len() as u16,
            payload_len: meta_len as u32,
        };
        self.push_with_wake(&end_header, &meta_buf)
    }

    fn send_reset(&mut self, stream_id: u32, code: StatusCode, msg: &str) -> Result<(), RpcError> {
        let msg_bytes = msg.as_bytes();
        let mut payload = Vec::with_capacity(8 + msg_bytes.len());
        payload.extend_from_slice(&(code as u32).to_le_bytes());
        payload.extend_from_slice(&(msg_bytes.len() as u32).to_le_bytes());
        payload.extend_from_slice(msg_bytes);
        let header = Header {
            stream_id,
            frame_type: FrameType::Reset,
            flags: Flags::END_STREAM,
            metadata_count: 0,
            payload_len: payload.len() as u32,
        };
        self.push_with_wake(&header, &payload)
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
                        return Err(RpcError::Transport("shmem: response too large for slot"));
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
                    let _ = self.signal_file.write_all(&[1u8]);
                    return Ok(());
                }
                Err(RingError::Full) => {
                    let _ = self.signal_file.write_all(&[1u8]);
                    std::thread::sleep(Duration::from_micros(10));
                }
                Err(RingError::FrameTooLarge) => {
                    return Err(RpcError::Transport("shmem: response too large for slot"));
                }
                Err(_) => return Err(RpcError::Transport("shmem: ring push error")),
            }
        }
    }
}

// ============================================================
// Server entry points
// ============================================================

impl Server {
    /// Bind a `transport-shmem` rendezvous at
    /// `rendezvous_path` and serve registered services
    /// forever.
    pub fn listen_shmem(&self, rendezvous_path: &Path) -> Result<(), RpcError> {
        let stop = AtomicBool::new(false);
        self.listen_shmem_until(rendezvous_path, &stop)
    }

    /// As [`Self::listen_shmem`], but exits when `stop`
    /// flips. Each accepted connection runs on its own
    /// OS thread.
    pub fn listen_shmem_until(
        &self,
        rendezvous_path: &Path,
        stop: &AtomicBool,
    ) -> Result<(), RpcError> {
        self.listen_shmem_inner(rendezvous_path, stop, false)
    }

    /// Same as [`Self::listen_shmem_until`] but runs every
    /// connection worker in **unbounded busy-poll mode**:
    /// the worker spins the recv ring without reading the
    /// wake fd and publishes responses without writing the
    /// wake fd. Throughput-mode peer; the client must call
    /// `ShmemConnection::set_busy_poll_unbounded(true)`
    /// to agree.
    ///
    /// Each connection burns one CPU on the server thread
    /// while in flight — appropriate for throughput probes
    /// or trusted bulk-transfer paths.
    pub fn listen_shmem_busy_poll_until(
        &self,
        rendezvous_path: &Path,
        stop: &AtomicBool,
    ) -> Result<(), RpcError> {
        self.listen_shmem_inner(rendezvous_path, stop, true)
    }

    fn listen_shmem_inner(
        &self,
        rendezvous_path: &Path,
        stop: &AtomicBool,
        busy_poll_unbounded: bool,
    ) -> Result<(), RpcError> {
        let listener = UnixListener::bind(rendezvous_path)
            .map_err(|_| RpcError::Transport("shmem: rendezvous bind failed"))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| RpcError::Transport("shmem: listener nonblocking failed"))?;

        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let _ = stream.set_nonblocking(false);
                    let server_clone = self.clone();
                    let h = std::thread::spawn(move || {
                        run_one_connection(stream, server_clone, busy_poll_unbounded);
                    });
                    handles.push(h);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(RpcError::Transport("shmem: accept failed")),
            }
            handles.retain(|h| !h.is_finished());
        }
        Ok(())
    }
}

fn run_one_connection(stream: UnixStream, server: Server, busy_poll_unbounded: bool) {
    let side = match accept_rendezvous(stream, SLOT_COUNT, SLOT_STRIDE) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut worker = ShmemConnWorker::new(side, busy_poll_unbounded);
    worker.serve(&server);
}
