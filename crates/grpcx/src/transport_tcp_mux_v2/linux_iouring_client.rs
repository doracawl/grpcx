//! v1.1 client-side io_uring fast path for the wire-v2
//! tcp-mux client — SQPOLL ring + SEND submits +
//! single-shot RECV into a rolling buffer.
//!
//! Symmetric counterpart to
//! [`super::linux_iouring_v2`]; landing this brings the
//! client side from three syscalls per unary RPC
//! (`writev` + 2 × `read_exact`) down to a single
//! `io_uring_enter(WAIT)` per CQE batch (SQPOLL drains
//! the submission queue without a syscall). Combined
//! with the v1.1 server, the loopback RTT path is one
//! syscall per side.
//!
//! Setup fallback chain (identical to the server side):
//!
//! 1. `Ring::new_sqpoll` (CAP_SYS_NICE or 5.13+
//!    unprivileged).
//! 2. `Ring::new` (plain ring, no SQPOLL — userspace pays
//!    one syscall per submit).
//! 3. `None` — caller falls back to the blocking
//!    `writev` + `read_exact` path that ships in
//!    [`super::connection`].
//!
//! The setup is **best-effort + sticky**: a successful
//! ring is held for the connection's lifetime; a setup
//! failure permanently disables the io_uring path for
//! that connection (`ring = None`).
//!
//! Per RFC `_dev/RFC-v1.1-iouring-sqpoll-server.md` § 5
//! follow-up: closing the < 8 µs gate requires symmetric
//! io_uring on both client and server.

#![cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]

use alloc::vec::Vec;
use core::time::Duration;
use std::time::Instant;

use grpcx_rt::iouring::{UringRing, drain_cqes, submit_recv, submit_send};

use crate::RpcError;
use crate::wire_v2::{HEADER_LEN, Header, decode_header};

/// User-data discriminators. The RECV tag is reused
/// across resubmits on the same RPC turn; SENDs are
/// tagged with a monotonic counter so the drain loop can
/// distinguish a stale send completion from the RECV's.
const UD_SEND_BASE: u64 = 0x100;
const UD_RECV: u64 = 0x02;

/// Per-connection rolling receive buffer. Sized to match
/// the v1.1 server's ring buffer so a single complete
/// response always fits.
const RECV_BUF_CAP: usize = 64 * 1024;

/// Number of SQ entries per ring. We submit at most two
/// SQEs per RPC turn (one SEND, one RECV) plus the
/// occasional PONG; 64 is comfortably above the
/// watermark.
const RING_ENTRIES: u32 = 64;

/// Client-side io_uring context. One per
/// `TcpMuxV2Connection`. Sticky once initialised — a
/// failed ring set-up is recorded by the caller as
/// `None` and never retried.
pub(super) struct ClientRing {
    ring: UringRing,
    fd: i32,
    /// `buf[head..tail]` is "valid but unparsed". The
    /// next RECV writes into `buf[tail..RECV_BUF_CAP]`.
    /// `head` advances past frames already returned to
    /// the caller; `compact` slides them out on demand.
    buf: Vec<u8>,
    head: usize,
    tail: usize,
    /// Per-connection scratch buffer for outbound
    /// frames. v1.1.1 leaked one Box<[u8]> per send;
    /// v1.2 owns a single 64 KiB Vec that's
    /// `clear()`-then-extended per send and held alive
    /// across the matching CQE — safe because
    /// `call_unary` strictly alternates send_frame →
    /// recv_frame (one SEND in flight at a time), and
    /// `recv_frame::drain_cqes` reaps the prior SEND CQE
    /// before the next call.
    send_buf: Vec<u8>,
    /// Whether a RECV SQE is currently pending in the
    /// kernel. We keep at most one outstanding.
    recv_in_flight: bool,
    /// Monotonic send-tag counter (for SQE user_data —
    /// purely for completion-time disambiguation).
    next_send_ud: u64,
    /// Set when any prior SEND completion reported an
    /// error. The next frame I/O surface checks this so
    /// the caller fails the RPC promptly.
    send_failed: bool,
    /// User-configurable busy-poll window (in
    /// microseconds). `> 0` makes `recv_frame` spin on
    /// the CQ ring for up to this duration before
    /// falling back to `io_uring_enter(WAIT)`. Default
    /// `0` — pure blocking-wait behaviour, no CPU spin.
    /// Matches the API shape of
    /// `ShmemConnection::set_busy_poll_threshold`.
    busy_poll_us: u64,
}

impl ClientRing {
    /// Attempt the SQPOLL → plain ring fallback chain.
    /// Returns `None` if both fail — the caller treats
    /// this as "io_uring unavailable, use the blocking
    /// path" and never retries.
    pub fn try_new(fd: i32) -> Option<Self> {
        // Plain ring (no SQPOLL) on the client side. The
        // server already pins its own SQPOLL thread to
        // CPU 0; when client and server share one
        // process (bench harness, in-process tests) two
        // SQPOLL threads on the same CPU serialise on
        // it and each RPC pays the scheduler quantum
        // (~4 ms). The single-syscall win — submit SEND
        // + RECV + wait via one `io_uring_enter(WAIT,
        // to_submit=2)` — does not require a kernel
        // poller, and avoids the cross-process /
        // cross-ring contention.
        //
        // A future v1.1.2 may opt into SQPOLL on the
        // client when the user supplies an explicit CPU
        // hint that doesn't collide with the server's
        // poller.
        let ring = UringRing::new(RING_ENTRIES).ok()?;

        let mut buf = Vec::with_capacity(RECV_BUF_CAP);
        // SAFETY: capacity == RECV_BUF_CAP from
        // `with_capacity`; the parser only ever touches
        // `buf[head..tail]`, and we only update `tail`
        // after a successful kernel write into the
        // window past `tail`.
        unsafe {
            buf.set_len(RECV_BUF_CAP);
        }

        Some(Self {
            ring,
            fd,
            buf,
            head: 0,
            tail: 0,
            send_buf: Vec::with_capacity(RECV_BUF_CAP),
            recv_in_flight: false,
            next_send_ud: UD_SEND_BASE,
            send_failed: false,
            busy_poll_us: 0,
        })
    }

    /// Set the busy-poll window. `Duration::ZERO`
    /// disables (default).
    pub fn set_busy_poll_threshold(&mut self, t: Duration) {
        self.busy_poll_us = t.as_micros() as u64;
    }

    /// Submit one outbound frame (already-encoded
    /// bytes including the 16 B header). v1.2 stages
    /// the bytes in the per-connection `send_buf` and
    /// hands the kernel a pointer into that buffer; the
    /// buffer stays alive until the next `recv_frame`
    /// drains the matching SEND CQE (which is
    /// guaranteed for unary because `call_unary`
    /// strictly alternates `send_frame` → `recv_frame`).
    ///
    /// Streaming RPCs are NOT yet routed through this
    /// path (the connection layer falls back to the
    /// blocking write_frame for `call_server_streaming`,
    /// `call_client_streaming`, `call_bidi`); when they
    /// move over, this method needs either a SEND-CQE
    /// wait between back-to-back sends or a buffer
    /// pool.
    pub fn send_frame(&mut self, frame: &[u8]) -> Result<(), RpcError> {
        self.send_buf.clear();
        self.send_buf.extend_from_slice(frame);
        let ptr = self.send_buf.as_ptr();
        let len = self.send_buf.len();
        let ud = self.next_send_ud;
        self.next_send_ud += 1;
        // SAFETY: `send_buf` is owned by `self` and not
        // moved or resized until the next `send_frame`
        // call (which happens only after `recv_frame`
        // drains the prior SEND CQE). The kernel reads
        // (does not write) `ptr`.
        unsafe {
            submit_send(&mut self.ring, self.fd, ptr, len as u32, 0, ud)
                .map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1 client: send submit failed"))?;
        }
        Ok(())
    }

    /// Receive one complete frame: header + payload
    /// returned as a borrow into the rolling buffer.
    /// Caller must consume the payload before the next
    /// `recv_frame` call (each call may compact /
    /// advance the buffer).
    pub fn recv_frame(&mut self) -> Result<(Header, Vec<u8>), RpcError> {
        // Header state for the *current* frame. Locked
        // in as soon as we have 16 B in the buffer.
        let mut got_header: Option<Header> = None;

        loop {
            // Short-circuit: parse from already-buffered
            // bytes before issuing a syscall. The first
            // RPC of a connection has nothing buffered;
            // subsequent RPCs may inherit residual bytes
            // from the previous frame's tail.
            if got_header.is_none() && self.tail - self.head >= HEADER_LEN {
                let mut hdr = [0u8; HEADER_LEN];
                hdr.copy_from_slice(&self.buf[self.head..self.head + HEADER_LEN]);
                got_header = Some(
                    decode_header(&hdr)
                        .map_err(|_| RpcError::Codec("tcp-mux-v2 v1.1 client: invalid header"))?,
                );
            }
            if let Some(h) = &got_header {
                let total = HEADER_LEN + h.payload_len as usize;
                if self.tail - self.head >= total {
                    let payload_start = self.head + HEADER_LEN;
                    let payload_end = payload_start + h.payload_len as usize;
                    let payload: Vec<u8> = self.buf[payload_start..payload_end].to_vec();
                    let header = *h;
                    self.head += total;
                    return Ok((header, payload));
                }
            }

            // Need more bytes — arm a RECV if not
            // already in flight, then advance the ring.
            //
            // **Blocking-wait path** (default,
            // `busy_poll_us == 0`): one `io_uring_enter`
            // syscall submits any pending SQEs and parks
            // until a CQE arrives. This is the
            // syscall-batching shape that gave v1.1.1
            // its 9 µs median — the wait itself is
            // where the dominant kernel-side overhead
            // sits (per `perf` profile: 21 % SQPOLL
            // task-work + 5.8 % `clear_bhb_loop` Spectre
            // mitigation + 2.6 % syscall entry).
            //
            // **Busy-poll path** (opt-in, `busy_poll_us
            // > 0`): submit pending SQEs without a wait
            // and spin on the CQ ring directly. The
            // CQ ring is mmap'd, so polling it is pure
            // memory access — no syscall, no Spectre
            // barrier. Trades 1 CPU core's worth of
            // busy loop for the wake-up syscall. Same
            // API + semantics as
            // `ShmemConnection::set_busy_poll_threshold`.
            self.arm_recv_if_needed()?;

            let mut recv_res: i32 = 0;
            let mut got_recv = false;
            let mut local_send_failed = false;

            if self.busy_poll_us > 0 {
                // Kick the ring without waiting.
                let _ = self.ring.submit(RING_ENTRIES);
                let deadline = Instant::now() + Duration::from_micros(self.busy_poll_us);
                loop {
                    drain_cqes(&mut self.ring, |c| {
                        if c.user_data == UD_RECV {
                            got_recv = true;
                            recv_res = c.res;
                        } else if c.user_data >= UD_SEND_BASE && c.res < 0 {
                            local_send_failed = true;
                        }
                    });
                    if got_recv || local_send_failed {
                        break;
                    }
                    if Instant::now() >= deadline {
                        // Spin exhausted — fall back to
                        // blocking wait. Submits is a
                        // no-op here (already
                        // submitted), so this is a pure
                        // park.
                        self.ring.submit_and_wait(0, 1).map_err(|_| {
                            RpcError::Transport("tcp-mux-v2 v1.1 client: wait failed")
                        })?;
                        drain_cqes(&mut self.ring, |c| {
                            if c.user_data == UD_RECV {
                                got_recv = true;
                                recv_res = c.res;
                            } else if c.user_data >= UD_SEND_BASE && c.res < 0 {
                                local_send_failed = true;
                            }
                        });
                        break;
                    }
                    core::hint::spin_loop();
                }
            } else {
                self.ring
                    .submit_and_wait(RING_ENTRIES, 1)
                    .map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1 client: wait failed"))?;
                drain_cqes(&mut self.ring, |c| {
                    if c.user_data == UD_RECV {
                        got_recv = true;
                        recv_res = c.res;
                    } else if c.user_data >= UD_SEND_BASE && c.res < 0 {
                        local_send_failed = true;
                    }
                });
            }

            if local_send_failed {
                self.send_failed = true;
            }
            if self.send_failed {
                return Err(RpcError::Transport(
                    "tcp-mux-v2 v1.1 client: SEND completion failed",
                ));
            }

            if got_recv {
                self.recv_in_flight = false;
                if recv_res < 0 {
                    return Err(RpcError::Transport(
                        "tcp-mux-v2 v1.1 client: recv completion failed",
                    ));
                }
                if recv_res == 0 {
                    return Err(RpcError::Transport("tcp-mux-v2 v1.1 client: peer closed"));
                }
                self.tail += recv_res as usize;
            }
            // Loop back to retry the parse.
        }
    }

    /// Slide unconsumed bytes to the front of the
    /// buffer when the tail region is too small for the
    /// next RECV. No-op when `head == 0`.
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

    /// Push a fresh RECV SQE into `buf[tail..cap]`
    /// if one is not already pending. Compacts first if
    /// the tail region would otherwise be empty.
    fn arm_recv_if_needed(&mut self) -> Result<(), RpcError> {
        if self.recv_in_flight {
            return Ok(());
        }
        if self.tail == RECV_BUF_CAP {
            self.compact();
        }
        if self.tail == RECV_BUF_CAP {
            return Err(RpcError::Transport(
                "tcp-mux-v2 v1.1 client: frame exceeds recv buffer",
            ));
        }
        let dst = self.buf[self.tail..RECV_BUF_CAP].as_mut_ptr();
        let len = (RECV_BUF_CAP - self.tail) as u32;
        // SAFETY: `dst` points inside our owned `buf`
        // for `len` bytes; the kernel only writes after
        // the matching CQE has been produced, and the
        // caller holds `&mut self` past the
        // `submit_and_wait` so the pointer stays valid.
        unsafe {
            submit_recv(&mut self.ring, self.fd, dst, len, 0, UD_RECV)
                .map_err(|_| RpcError::Transport("tcp-mux-v2 v1.1 client: recv submit failed"))?;
        }
        self.recv_in_flight = true;
        Ok(())
    }
}
