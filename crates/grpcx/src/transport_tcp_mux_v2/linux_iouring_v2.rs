//! v1.1 Linux io_uring fast path for the wire-v2 tcp-mux
//! server — SQPOLL ring + cqe-driven event loop +
//! linked-send pair per response frame.
//!
//! Supersedes the v1 [`super::linux_iouring`] when its
//! own setup succeeds; fallback chain on setup failure:
//!
//! 1. `Ring::new_sqpoll` (CAP_SYS_NICE or 5.13+ unprivileged).
//! 2. `Ring::new` (plain ring, no SQPOLL — userspace pays
//!    one syscall per submit, otherwise same loop shape).
//! 3. `Err((stream, RpcError))` — caller falls back to the
//!    T4.1 blocking serve via `super::server::serve_one`.
//!
//! Per RFC `_dev/RFC-v1.1-iouring-sqpoll-server.md`. Target
//! gate: median tcp-mux v2 unary RTT < 8 µs on lx64
//! (Linux 6.x x86_64).
//!
//! Hot-path design:
//!
//! - **Single recv in flight per connection** (no
//!   multishot, no BUF_RING — option A of RFC § 3.4).
//!   Simple memory model: kernel writes into the per-
//!   connection rolling buffer's tail region; userspace
//!   drains the CQE, parses any complete frames, then
//!   resubmits a fresh recv pointing at the new tail.
//! - **Linked SEND pair** for the response header +
//!   payload. The kernel guarantees in-order delivery on
//!   the same TCP fd regardless of the link, so the link
//!   is purely for cancel-on-error semantics.
//! - **`submit_and_wait(0, 1)`** parks between CQE
//!   batches. With SQPOLL active, ordinary submits skip
//!   the syscall; only the wait costs one
//!   `io_uring_enter`.

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

/// User-data discriminators. The recv tag has one
/// outstanding value; sends use a per-frame counter so
/// the cqe-drain loop can match completion to the
/// originating header/payload (we ignore the match
/// today — only the result code matters — but the
/// counter prevents ambiguity if a future batch sends
/// multiple frames in flight).
const UD_RECV: u64 = 0x01;
const UD_SEND_BASE: u64 = 0x100;

/// Per-connection rolling receive buffer.
const RECV_BUF_CAP: usize = 64 * 1024;

/// SQPOLL configuration. The serve thread sets its own
/// affinity to the same CPU; the kernel polling thread
/// shares the core but is mostly waiting on SQE writes.
const SQPOLL_IDLE_MS: u32 = 100;

struct StreamState {
    service: String,
    method: String,
    user_meta: Metadata,
    request_bodies: Vec<Vec<u8>>,
}

/// Connection-scoped serve state. Owned per accepted
/// `TcpStream`; lives for the connection's lifetime.
/// `_stream` is held by value so the underlying fd
/// stays open for the kernel-side io_uring ops; `fd`
/// caches the raw value for cheap reuse in SQEs.
struct Conn {
    ring: UringRing,
    _stream: TcpStream,
    fd: i32,
    /// `buf[..tail]` holds valid bytes from prior recvs.
    /// `buf[tail..RECV_BUF_CAP]` is the write target for
    /// the next outstanding recv.
    buf: Vec<u8>,
    tail: usize,
    /// Frames already parsed live before `head`. The
    /// region `[head..tail]` is "valid but not yet
    /// parsed" — bytes belonging to incomplete frames
    /// at the tail.
    head: usize,
    streams: BTreeMap<u32, StreamState>,
    /// v1.2 send buffer pool. Slot `i` is identified on
    /// the wire by `user_data = UD_SEND_BASE + i`; each
    /// slot owns its `Vec<u8>` for the entire connection
    /// lifetime. v1.1 leaked one `Box<[u8]>` per send;
    /// the pool replaces that with `RING_ENTRIES` reusable
    /// scratch buffers indexed by send completion.
    send_slots: Vec<Vec<u8>>,
    free_slots: Vec<usize>,
    /// Track whether a recv is outstanding so we don't
    /// double-submit.
    recv_in_flight: bool,
}

impl Conn {
    fn new(stream: TcpStream) -> Result<Self, (TcpStream, RpcError)> {
        let fd = stream.as_raw_fd();
        // Try SQPOLL first; fall back to plain ring on
        // EPERM / kernel refusal. The CPU pinning value
        // is a placeholder — single-thread bench
        // doesn't have a per-thread CPU plan yet; v1.1
        // pins to CPU 0 by default and lets the
        // scheduler do its thing. Multi-thread fan-out
        // is a follow-up.
        let ring = match UringRing::new_sqpoll(64, 0, SQPOLL_IDLE_MS) {
            Ok(r) => r,
            Err(_) => match UringRing::new(64) {
                Ok(r) => r,
                Err(_) => {
                    return Err((
                        stream,
                        RpcError::Transport("tcp-mux-v2 v1.1: ring setup failed"),
                    ));
                }
            },
        };

        // Pre-size the receive buffer to capacity. The
        // bytes past `tail` are uninitialised until the
        // kernel writes them; we never read past `tail`,
        // so the uninit window is safe.
        let mut buf = Vec::with_capacity(RECV_BUF_CAP);
        // SAFETY: capacity == RECV_BUF_CAP from
        // `with_capacity`; setting the length over the
        // raw allocation exposes uninit bytes that the
        // kernel will overwrite before we read them
        // (the parser only ever touches
        // `buf[head..tail]`, and we only update `tail`
        // after a successful recv).
        unsafe {
            buf.set_len(RECV_BUF_CAP);
        }

        // Pool of `RING_ENTRIES` reusable send-scratch
        // buffers. Each slot starts empty; capacity
        // grows on demand the first time it carries a
        // frame. Caps the in-flight send-buffer count
        // at the SQ depth — same upper bound as the
        // ring's own SQE budget.
        const POOL_SIZE: usize = 64;
        let mut send_slots: Vec<Vec<u8>> = Vec::with_capacity(POOL_SIZE);
        let mut free_slots: Vec<usize> = Vec::with_capacity(POOL_SIZE);
        for i in 0..POOL_SIZE {
            send_slots.push(Vec::new());
            // Stack-like — pop from the end first, so
            // slot 0 is allocated last and most CQEs
            // identify the same high slots.
            free_slots.push(POOL_SIZE - 1 - i);
        }

        Ok(Self {
            ring,
            _stream: stream,
            fd,
            buf,
            tail: 0,
            head: 0,
            streams: BTreeMap::new(),
            send_slots,
            free_slots,
            recv_in_flight: false,
        })
    }

    /// Slide unconsumed bytes to the front of the buffer
    /// when the tail can no longer fit another recv. No-op
    /// when `head == 0`.
    fn compact(&mut self) {
        if self.head == 0 {
            return;
        }
        if self.head == self.tail {
            self.head = 0;
            self.tail = 0;
        } else {
            self.buf.copy_within(self.head..self.tail, 0);
            self.tail -= self.head;
            self.head = 0;
        }
    }

    /// Submit a fresh recv into `buf[tail..RECV_BUF_CAP]`
    /// if one is not already in flight.
    fn arm_recv(&mut self) -> Result<(), RpcError> {
        if self.recv_in_flight {
            return Ok(());
        }
        if self.tail == RECV_BUF_CAP {
            self.compact();
        }
        if self.tail == RECV_BUF_CAP {
            // Buffer is full and compaction did nothing
            // — a single frame exceeded RECV_BUF_CAP.
            // v1.1 caps frames at the buffer size; this
            // is a peer protocol violation.
            return Err(RpcError::Transport(
                "tcp-mux-v2 v1.1: frame exceeds recv buffer",
            ));
        }
        let dst = self.buf[self.tail..RECV_BUF_CAP].as_mut_ptr();
        let len = (RECV_BUF_CAP - self.tail) as u32;
        // SAFETY: `dst` points inside our owned `buf`;
        // `len` is the remaining capacity. The pointer
        // stays valid until the matching CQE is drained
        // (we hold `&mut self` until we yield to the
        // wait, and the kernel only writes after the
        // submit completes).
        unsafe {
            submit_recv(&mut self.ring, self.fd, dst, len, 0, UD_RECV)
                .map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1: recv submit failed"))?;
        }
        self.recv_in_flight = true;
        Ok(())
    }
}

/// Per-connection v1.1 serve loop. See the module-level
/// doc for the failure / fallback contract.
pub(super) fn try_serve_iouring_v2(
    stream: TcpStream,
    server: &Server,
) -> Result<(), (TcpStream, RpcError)> {
    let mut conn = Conn::new(stream)?;
    let _ = drive_loop(&mut conn, server);
    // Conn drops here — ring drop cancels any
    // outstanding kernel ops and closes the io_uring
    // fd; `_stream` drop closes the connection's TCP
    // socket fd.
    Ok(())
}

fn drive_loop(conn: &mut Conn, server: &Server) -> Result<(), RpcError> {
    conn.arm_recv()?;
    loop {
        // Flush any pending sends + the initial recv.
        // With SQPOLL the kernel thread drains the SQ;
        // we just nudge it (no syscall on the fast
        // path).
        let _ = conn.ring.submit(0);

        // Park until at least one completion arrives.
        let _ = conn
            .ring
            .submit_and_wait(0, 1)
            .map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1: wait failed"))?;

        // Drain every available CQE. Collect actions
        // first so we don't mutate `conn` while
        // `drain_cqes` borrows the ring.
        let mut recv_results: Vec<i32> = Vec::new();
        let mut completed_send_slots: Vec<usize> = Vec::new();
        let mut send_errors: bool = false;
        drain_cqes(&mut conn.ring, |c| {
            if c.user_data == UD_RECV {
                recv_results.push(c.res);
            } else if c.user_data >= UD_SEND_BASE {
                let slot_idx = (c.user_data - UD_SEND_BASE) as usize;
                completed_send_slots.push(slot_idx);
                if c.res < 0 {
                    send_errors = true;
                }
            }
        });
        // Return completed send buffers to the pool so
        // subsequent `send_frame` calls can reuse them.
        for idx in completed_send_slots {
            conn.free_slots.push(idx);
        }

        // Handle recv result first — most CQE batches
        // are exactly one recv + zero or more send
        // completions.
        for res in recv_results {
            conn.recv_in_flight = false;
            if res < 0 {
                // -errno (peer reset / etc.) → drop conn.
                return Ok(());
            }
            if res == 0 {
                // Peer closed — clean exit.
                return Ok(());
            }
            conn.tail += res as usize;
            // Parse any complete frames now sitting in
            // `buf[head..tail]`.
            while let Some(header) = peek_header(conn)? {
                let frame_len = HEADER_LEN + header.payload_len as usize;
                if conn.tail - conn.head < frame_len {
                    break; // payload incomplete
                }
                handle_frame(conn, server, header)?;
                conn.head += frame_len;
            }
            // Re-arm recv for the next slice.
            conn.arm_recv()?;
        }

        if send_errors {
            // Cancel-on-error from the linked SEND chain
            // — drop the connection.
            return Ok(());
        }
    }
}

/// Try to decode the next frame's header from
/// `buf[head..tail]`. Returns `None` when fewer than 16
/// bytes are buffered, `Err` on a malformed header.
fn peek_header(conn: &Conn) -> Result<Option<Header>, RpcError> {
    if conn.tail - conn.head < HEADER_LEN {
        return Ok(None);
    }
    let mut hdr = [0u8; HEADER_LEN];
    hdr.copy_from_slice(&conn.buf[conn.head..conn.head + HEADER_LEN]);
    let header =
        decode_header(&hdr).map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1: invalid header"))?;
    Ok(Some(header))
}

fn handle_frame(conn: &mut Conn, server: &Server, header: Header) -> Result<(), RpcError> {
    let payload_start = conn.head + HEADER_LEN;
    let payload_end = payload_start + header.payload_len as usize;
    // Clone the payload to an owned Vec — the recv
    // buffer's bytes may be overwritten once we arm the
    // next recv inside the loop, so any state stored in
    // `streams` must own its data.
    let payload: Vec<u8> = conn.buf[payload_start..payload_end].to_vec();

    match header.frame_type {
        FrameType::Begin => handle_begin(conn, server, header, payload),
        FrameType::Data => handle_data(conn, server, header, payload),
        FrameType::End => {
            if let Some(st) = conn.streams.remove(&header.stream_id) {
                dispatch_stream(conn, server, header.stream_id, st)
            } else {
                send_reset(
                    conn,
                    header.stream_id,
                    StatusCode::Internal,
                    "END on unknown stream",
                )
            }
        }
        FrameType::Reset => {
            conn.streams.remove(&header.stream_id);
            Ok(())
        }
        FrameType::Ping => {
            let pong = Header {
                stream_id: 0,
                frame_type: FrameType::Pong,
                flags: Flags::NONE,
                metadata_count: 0,
                payload_len: payload.len() as u32,
            };
            send_frame(conn, &pong, &payload)
        }
        FrameType::Pong => Ok(()),
    }
}

fn handle_begin(
    conn: &mut Conn,
    server: &Server,
    header: Header,
    payload: Vec<u8>,
) -> Result<(), RpcError> {
    let (entries, body) = if header.flags.contains(Flags::HAS_METADATA) {
        decode_metadata_block(&payload, header.metadata_count)
            .map_err(|_| RpcError::Codec("tcp-mux-v2 v1.1: metadata decode failed"))?
    } else {
        (Vec::new(), payload.as_slice())
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
            // informational
        } else {
            user_meta.headers.push((
                String::from_utf8_lossy(e.name).into_owned(),
                e.value.to_vec(),
            ));
        }
    }

    if service.is_empty() || method.is_empty() {
        return send_reset(
            conn,
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
        dispatch_stream(conn, server, header.stream_id, st)
    } else {
        conn.streams.insert(header.stream_id, st);
        Ok(())
    }
}

fn handle_data(
    conn: &mut Conn,
    server: &Server,
    header: Header,
    payload: Vec<u8>,
) -> Result<(), RpcError> {
    let dispatch_now = if let Some(st) = conn.streams.get_mut(&header.stream_id) {
        if !payload.is_empty() {
            st.request_bodies.push(payload);
        }
        header.flags.contains(Flags::END_STREAM)
    } else {
        return send_reset(
            conn,
            header.stream_id,
            StatusCode::Internal,
            "DATA on unknown stream",
        );
    };

    if dispatch_now {
        let st = conn.streams.remove(&header.stream_id).unwrap();
        dispatch_stream(conn, server, header.stream_id, st)?;
    }
    Ok(())
}

fn dispatch_stream(
    conn: &mut Conn,
    server: &Server,
    stream_id: u32,
    st: StreamState,
) -> Result<(), RpcError> {
    let svc = match server.find_service(&st.service) {
        Some(s) => s,
        None => {
            return send_reset(
                conn,
                stream_id,
                StatusCode::Unimplemented,
                "service not registered",
            );
        }
    };

    let mut ctx = CallContext::new(st.user_meta);
    // TCP carries no peer credentials.

    match svc.dispatch_with_ctx(&st.method, &mut ctx, &st.request_bodies) {
        Ok(resp_bodies) => {
            let trailing = ctx.into_trailing();
            send_response(conn, stream_id, resp_bodies, trailing)
        }
        Err(RpcError::Status(s)) => send_reset_status(conn, stream_id, s),
        Err(_) => send_reset(conn, stream_id, StatusCode::Internal, "handler error"),
    }
}

fn send_response(
    conn: &mut Conn,
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
        return send_frame_two_parts(conn, &header, &meta_buf, &body);
    }

    for body in &bodies {
        let header = Header {
            stream_id,
            frame_type: FrameType::Data,
            flags: Flags::NONE,
            metadata_count: 0,
            payload_len: body.len() as u32,
        };
        send_frame(conn, &header, body)?;
    }
    let end_header = Header {
        stream_id,
        frame_type: FrameType::End,
        flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
        metadata_count: trailing_entries.len() as u16,
        payload_len: meta_len as u32,
    };
    send_frame(conn, &end_header, &meta_buf)
}

/// Submit one combined SEND covering the 16 B header
/// followed by the payload, so each v2 frame lands in
/// the kernel as a single op. v1 of this serve used
/// `submit_linked_send_pair` (header SQE LINK-chained
/// to body SQE) which works for a single frame but
/// loses chain-to-chain ordering when multi-frame
/// responses (server-streaming, bidi, RESET-then-END)
/// queue several chains back-to-back: each pair-chain
/// is independent, so the kernel may interleave bodies
/// from different frames on the same TCP socket. The
/// single-SEND-per-frame shape lets the kernel preserve
/// per-frame atomicity via SQE submission order.
///
/// v1.2: the frame is staged in a recycled buffer slot
/// (`conn.send_slots[idx]`) rather than a per-frame
/// leaked `Box`; the slot is reclaimed when its SEND
/// CQE drains (see `drive_loop`). Slot exhaustion is
/// fatal — the SQ depth is the upper bound on
/// in-flight sends anyway, so an exhausted pool means
/// the SQ is also full.
fn send_frame(conn: &mut Conn, header: &Header, payload: &[u8]) -> Result<(), RpcError> {
    let slot_idx = conn.free_slots.pop().ok_or(RpcError::Transport(
        "tcp-mux-v2 v1.1: send slot pool exhausted",
    ))?;
    let slot = &mut conn.send_slots[slot_idx];
    slot.clear();
    slot.resize(HEADER_LEN + payload.len(), 0);
    {
        // SAFETY: `slot` was just resized to at least
        // `HEADER_LEN` bytes.
        let hdr_slot: &mut [u8; HEADER_LEN] = (&mut slot[..HEADER_LEN]).try_into().unwrap();
        encode_header(header, hdr_slot);
    }
    slot[HEADER_LEN..].copy_from_slice(payload);

    let ptr = slot.as_ptr();
    let total_len = slot.len();
    let ud = UD_SEND_BASE + slot_idx as u64;

    // SAFETY: the slot is owned by `conn.send_slots`
    // and not resized or freed until its CQE has been
    // observed (the drive_loop's drain pushes the slot
    // back into `free_slots` only on CQE arrival). The
    // kernel reads (does not write) `ptr`.
    let submit_res = unsafe { submit_send(&mut conn.ring, conn.fd, ptr, total_len as u32, 0, ud) };
    if submit_res.is_err() {
        // On submit failure, return the slot so we don't
        // leak it from the pool.
        conn.free_slots.push(slot_idx);
        return Err(RpcError::Transport("tcp-mux-v2 v1.1: send submit failed"));
    }
    Ok(())
}

/// Two-part payload (e.g. metadata block + inline body).
/// Concatenates into a single contiguous send via
/// [`send_frame`].
fn send_frame_two_parts(
    conn: &mut Conn,
    header: &Header,
    part_a: &[u8],
    part_b: &[u8],
) -> Result<(), RpcError> {
    let mut combined = Vec::with_capacity(part_a.len() + part_b.len());
    combined.extend_from_slice(part_a);
    combined.extend_from_slice(part_b);
    send_frame(conn, header, &combined)
}

fn send_reset(
    conn: &mut Conn,
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
    send_frame(conn, &header, &payload)
}

fn send_reset_status(conn: &mut Conn, stream_id: u32, status: Status) -> Result<(), RpcError> {
    send_reset(conn, stream_id, status.code, &status.message)
}

fn encode_reset_payload(code: StatusCode, msg: &str) -> Vec<u8> {
    let msg_bytes = msg.as_bytes();
    let mut out = Vec::with_capacity(8 + msg_bytes.len());
    out.extend_from_slice(&(code as u32).to_le_bytes());
    out.extend_from_slice(&(msg_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(msg_bytes);
    out
}
