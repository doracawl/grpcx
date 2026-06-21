// Transport-agnostic runtime seam, shared by the H/3 and
// H/2 wirings (extracted from `transport_h3.rs` when the
// h2c interop bridge landed — charter 2026-06-11):
//
// - [`DatagramTransport`] — the IO seam. "Datagram" is
//   chunk semantics, not UDP: QUIC datagrams and TCP
//   byte-stream segments both fit (the pumps are
//   chunk-oriented and reassemble internally).
// - `drive_once` — one pump-send-recv cycle with the
//   spin-then-park idle policy (reactor-backed transports
//   sleep in the kernel via `FdParker`; queue pairs yield).
// - [`ConnectedChannel`] — channel + transport bundled,
//   the tonic-habit client handle generated stubs take.
// - [`call_unary`] — the blocking facade, dispatching on
//   the channel's negotiated transport.

use crate::{Metadata, Method, RpcError};
use alloc::boxed::Box;
use alloc::vec::Vec;

// ============================================================
// The IO seam.
// ============================================================

/// The chunk shuttle a blocking call drives — the only IO
/// abstraction L10 needs. Implementations: in-memory queue
/// pairs (tests / loopback), UDP sockets (`grpcx/h3`), TCP
/// sockets (h2c).
pub trait DatagramTransport {
    /// Send one chunk toward the peer.
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError>;
    /// Receive one chunk if available. `Ok(None)` means
    /// "nothing right now" (the caller decides whether to
    /// spin, sleep, or give up — see [`call_unary`]).
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError>;

    /// Send a burst of chunks. Default: one
    /// [`send`](Self::send) per chunk. Implementations
    /// with a kernel batch path (`UdpTransport` on Darwin:
    /// `sendmsg_x`) override to move the burst in one
    /// syscall — bulk responses are emitted as dozens of
    /// MTU-sized datagrams per pump, and per-datagram
    /// syscalls dominate the large-payload lanes.
    fn send_batch(&mut self, datagrams: &[Vec<u8>]) -> Result<(), RpcError> {
        for d in datagrams {
            self.send(d)?;
        }
        Ok(())
    }

    /// Receive a burst of chunks into `out`, returning how
    /// many arrived (0 = nothing right now). Default: one
    /// [`recv`](Self::recv).
    fn recv_batch(&mut self, out: &mut Vec<Vec<u8>>) -> Result<usize, RpcError> {
        match self.recv()? {
            Some(d) => {
                out.push(d);
                Ok(1)
            }
            None => Ok(0),
        }
    }

    /// Park until the peer's next chunk may have arrived,
    /// or `timeout` elapses. Drive loops call this once
    /// their brief spin budget is exhausted, so a quiet
    /// link sleeps in the kernel and wakes the moment data
    /// lands — instead of yield-thrashing, where every
    /// cycle pays a scheduler quantum plus an empty recv
    /// syscall that contends with the peer's send path.
    /// Returns `true` if the wait actually parked in a
    /// reactor; the default yields once and returns
    /// `false` (in-memory queue pairs have no OS handle to
    /// park on).
    fn wait_readable(&mut self, _timeout: core::time::Duration) -> Result<bool, RpcError> {
        std::thread::yield_now();
        Ok(false)
    }
}

// ============================================================
// Reactor parking (the event-driven idle path).
// ============================================================

/// One-fd reactor handle a socket transport parks on when
/// its link goes quiet: kqueue on Darwin / FreeBSD, epoll
/// on Linux, via L1's [`grpcx_io::Poll`]. Built lazily on
/// the first park so transports multiplexed by an outer
/// loop (server conns) don't each hold a kernel queue they
/// never sleep in.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
pub(crate) struct FdParker {
    poll: grpcx_io::Poll,
    events: grpcx_io::Events,
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
impl FdParker {
    /// Watch `fd` for readability. The registration is
    /// edge-shaped on both backends (`EV_CLEAR` /
    /// `EPOLLET`), which is lost-wakeup-safe here because
    /// every drive loop drains its socket to `WouldBlock`
    /// before parking — an arrival after the drain is a
    /// fresh edge, and an fd that is already readable at
    /// registration time delivers an initial event on both
    /// kernels.
    pub(crate) fn new(fd: grpcx_io::RawFd) -> Result<Self, RpcError> {
        let poll =
            grpcx_io::Poll::new().map_err(|_| RpcError::Transport("reactor: create failed"))?;
        let mut src = grpcx_io::SourceFd::new(fd);
        poll.registry()
            .register(&mut src, grpcx_io::Token(0), grpcx_io::Interest::READABLE)
            .map_err(|_| RpcError::Transport("reactor: register failed"))?;
        Ok(FdParker {
            poll,
            events: grpcx_io::Events::with_capacity(4),
        })
    }

    /// Sleep until the fd goes readable or `timeout`
    /// elapses (the caller re-drains either way, so a
    /// timeout is not an error).
    pub(crate) fn park(&mut self, timeout: core::time::Duration) -> Result<(), RpcError> {
        self.poll
            .poll(&mut self.events, Some(timeout))
            .map_err(|_| RpcError::Transport("reactor: poll failed"))
    }
}

/// How long one reactor park sleeps before the drive loop
/// rechecks the link. Bounds server stop-flag latency and
/// the cadence at which transport timers (PTO, keepalive,
/// idle timeout) get an idle pump.
pub(crate) const PARK_SLICE: core::time::Duration = core::time::Duration::from_micros(500);

impl crate::Channel {
    /// Feed one incoming chunk (or `None` to let timers /
    /// handshake progress) and collect outgoing chunks —
    /// datagrams on H/3, byte-stream segments on h2c.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        #[cfg(feature = "transport-h3")]
        if self.h3.is_some() {
            return crate::transport_h3::pump_h3(self, incoming);
        }
        #[cfg(feature = "transport-h2")]
        if self.h2.is_some() {
            return crate::transport_h2::pump_h2(self, incoming);
        }
        #[cfg(feature = "transport-tcp-mux")]
        if let Some(i) = self.tcpmux.as_mut() {
            return i.pump(incoming);
        }
        #[cfg(feature = "transport-ws-mux")]
        if let Some(i) = self.wsmux.as_mut() {
            return i.pump(incoming);
        }
        Err(RpcError::TransportUnavailable)
    }

    /// Tune this channel's link-liveness atom (defaults:
    /// 2.5 s keepalive, 10 s idle timeout), whatever the
    /// negotiated transport: a quiet link gets a
    /// transport-level probe each `keepalive` interval and
    /// dies with a `Transport("…: link dead")` error after
    /// `idle_timeout` with no inbound traffic. Liveness is
    /// the transport's job; per-call time limits are the
    /// caller's (the deadline surface).
    pub fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) -> Result<(), RpcError> {
        #[cfg(feature = "transport-h3")]
        if self.h3.is_some() {
            return self.set_link_liveness_h3(keepalive, idle_timeout);
        }
        #[cfg(feature = "transport-h2")]
        if let Some(i) = self.h2.as_mut() {
            i.set_link_liveness(keepalive, idle_timeout);
            return Ok(());
        }
        #[cfg(feature = "transport-tcp-mux")]
        if let Some(i) = self.tcpmux.as_mut() {
            i.set_link_liveness(keepalive, idle_timeout);
            return Ok(());
        }
        #[cfg(feature = "transport-ws-mux")]
        if let Some(i) = self.wsmux.as_mut() {
            i.set_link_liveness(keepalive, idle_timeout);
            return Ok(());
        }
        let _ = (keepalive, idle_timeout);
        Err(RpcError::TransportUnavailable)
    }

    /// `true` once the transport's handshake completed and
    /// RPCs can flow.
    pub fn is_ready(&self) -> bool {
        #[cfg(feature = "transport-h3")]
        if let Some(i) = self.h3.as_ref() {
            return i.is_ready();
        }
        #[cfg(feature = "transport-h2")]
        if let Some(i) = self.h2.as_ref() {
            return i.is_ready();
        }
        #[cfg(feature = "transport-tcp-mux")]
        if let Some(i) = self.tcpmux.as_ref() {
            return i.is_ready();
        }
        #[cfg(feature = "transport-ws-mux")]
        if let Some(i) = self.wsmux.as_ref() {
            return i.is_ready();
        }
        false
    }
}

/// The caller's per-call deadline, parsed once from the
/// request metadata's `grpc-timeout` (gRPC semantics:
/// counted from call start, connection establishment
/// included; no header = no deadline). Checked each drive
/// cycle by the blocking facades — expiry is a CALL
/// verdict (`Status(DEADLINE_EXCEEDED)`), never a link
/// verdict: the connection stays usable.
pub(crate) struct CallDeadline(Option<std::time::Instant>);

impl CallDeadline {
    pub(crate) fn from_metadata(meta: &Metadata) -> Self {
        CallDeadline(crate::grpc_timeout_of(meta).map(|d| std::time::Instant::now() + d))
    }

    pub(crate) fn check(&self) -> Result<(), RpcError> {
        match self.0 {
            Some(d) if std::time::Instant::now() >= d => Err(RpcError::Status(crate::Status {
                code: crate::StatusCode::DeadlineExceeded,
                message: alloc::string::String::from("deadline exceeded"),
            })),
            _ => Ok(()),
        }
    }
}

/// One pump-send-recv cycle. Returns the updated idle
/// counter (it only drives the spin-then-park policy —
/// link death is the TRANSPORT's verdict now, surfacing as
/// a pump error from its keepalive/idle-timeout atom; the
/// caller's time limit is the deadline surface).
/// The connected socket surfaced an ICMP port-unreachable
/// (ECONNREFUSED) — nobody is listening at the peer.
/// `drive_once` fast-fails on it while the channel is
/// handshaking and swallows it as transient once
/// established (charter: REFACTOR-v10-econnrefused.md).
pub(crate) const UDP_REFUSED: &str = "udp: connection refused";

pub(crate) fn drive_once(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    idle: u32,
) -> Result<u32, RpcError> {
    // ECONNREFUSED policy (charter:
    // REFACTOR-v10-econnrefused.md): while HANDSHAKING,
    // a refused datagram means nobody is listening —
    // fast-fail (TCP-connect parity). Once ESTABLISHED, a
    // stray ICMP (server restart window, off-path noise)
    // is transient: swallow it and let the liveness plane
    // / the caller's deadline own the death verdict.
    let refused_is_fatal = !channel.is_ready();
    let gate = |r: Result<(), RpcError>, fatal: bool| match r {
        Err(RpcError::Transport(m)) if m == UDP_REFUSED && !fatal => Ok(()),
        other => other,
    };
    let mut progressed = false;
    let outs = channel.pump(None)?;
    if !outs.is_empty() {
        gate(transport.send_batch(&outs), refused_is_fatal)?;
        progressed = true;
    }
    // Drain the whole receive queue before yielding — a
    // multi-MTU response arrives as a burst of datagrams,
    // and one-per-cycle processing serializes each behind
    // a scheduler round-trip. Batched both ways: one
    // syscall pulls a burst in, replies accumulate and go
    // out in one syscall.
    let mut inbox: Vec<Vec<u8>> = Vec::new();
    let mut replies: Vec<Vec<u8>> = Vec::new();
    loop {
        let n = match transport.recv_batch(&mut inbox) {
            Ok(n) => n,
            Err(RpcError::Transport(m)) if m == UDP_REFUSED && !refused_is_fatal => {
                // Transient: the queued ICMP error is
                // consumed; poll again next cycle.
                break;
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        for d in inbox.drain(..) {
            replies.extend(channel.pump(Some(&d))?);
        }
        progressed = true;
    }
    if !replies.is_empty() {
        gate(transport.send_batch(&replies), refused_is_fatal)?;
    }
    if progressed {
        Ok(0)
    } else if idle >= 4 {
        // Spin budget exhausted: park in the reactor until
        // the socket goes readable (or one slice, so the
        // transport's timers keep getting pumped). Memory
        // transports keep the yield default.
        transport.wait_readable(PARK_SLICE)?;
        Ok(idle.saturating_add(1))
    } else {
        // A few spin cycles before parking, for chunks
        // already in flight. Budget history (all A/B
        // measured): 512 and 128 WORSE than 64 (empty
        // recv syscalls contend with the peer's send path
        // in-kernel); 4 BEATS 64 on every lane (16 B
        // -31 %, 64 KiB -8 % — the v0.15 syscall ledger
        // showed 66.8 empty recvs ≈ 41 µs/call at 64);
        // 1 ≈ 4, so 4 keeps a small margin for sub-µs
        // arrivals at no measurable cost.
        Ok(idle + 1)
    }
}

// ============================================================
// Blocking unary facade.
// ============================================================

/// Issue a unary RPC, blocking until the response arrives:
/// drives `channel` against `transport` (handshake
/// included) until the call completes or the link stalls.
/// Dispatches on the channel's negotiated transport
/// (`grpcx/h3` or h2c).
///
/// # Errors
/// - [`RpcError::Status`] — the server answered with a
///   non-`Ok` gRPC status.
/// - [`RpcError::Transport`] — link-level failure,
///   including a stall (65 536 consecutive idle cycles
///   with nothing sent, received, or completed).
/// - [`RpcError::Codec`] — malformed response wire data.
pub fn call_unary(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    #[cfg(feature = "transport-h2")]
    if channel.h2.is_some() {
        return crate::transport_h2::call_unary_h2(channel, transport, method, metadata, request);
    }
    #[cfg(feature = "transport-tcp-mux")]
    if channel.tcpmux.is_some() {
        return crate::tcpmux::call_unary_tcp_mux(channel, transport, method, metadata, request);
    }
    #[cfg(feature = "transport-h3")]
    if channel.h3.is_some() {
        return crate::transport_h3::call_unary_h3(channel, transport, method, metadata, request);
    }
    #[cfg(feature = "transport-ws-mux")]
    if channel.wsmux.is_some() {
        return crate::wsmux::call_unary_ws_mux(channel, transport, method, metadata, request);
    }
    Err(RpcError::TransportUnavailable)
}

/// The 0-or-1-message read shape shared by the unary /
/// client-streaming facades over a batch core.
#[cfg(any(
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
pub(crate) fn first_or_empty(mut msgs: Vec<Vec<u8>>) -> Vec<u8> {
    if msgs.is_empty() {
        Vec::new()
    } else {
        msgs.swap_remove(0)
    }
}

/// Server-streaming RPC, blocking: one request out, every
/// response message collected until the stream completes.
/// Dispatches on the channel's negotiated transport.
pub fn call_server_streaming(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    #[cfg(feature = "transport-h2")]
    if channel.h2.is_some() {
        return crate::transport_h2::call_streaming_h2(
            channel,
            transport,
            method,
            metadata,
            &[request],
        );
    }
    #[cfg(feature = "transport-tcp-mux")]
    if channel.tcpmux.is_some() {
        return crate::tcpmux::call_streaming_tcp_mux(
            channel,
            transport,
            method,
            metadata,
            &[request],
        );
    }
    #[cfg(feature = "transport-h3")]
    if channel.h3.is_some() {
        return crate::transport_h3::call_server_streaming_h3(
            channel, transport, method, metadata, request,
        );
    }
    #[cfg(feature = "transport-ws-mux")]
    if channel.wsmux.is_some() {
        return crate::wsmux::call_streaming_ws_mux(
            channel,
            transport,
            method,
            metadata,
            &[request],
        );
    }
    Err(RpcError::TransportUnavailable)
}

/// Client-streaming RPC, blocking: every request message
/// out, then the single response. Dispatches on the
/// channel's negotiated transport.
pub fn call_client_streaming(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    #[cfg(feature = "transport-h2")]
    if channel.h2.is_some() {
        let (msgs, trailing) =
            crate::transport_h2::call_streaming_h2(channel, transport, method, metadata, requests)?;
        return Ok((first_or_empty(msgs), trailing));
    }
    #[cfg(feature = "transport-tcp-mux")]
    if channel.tcpmux.is_some() {
        let (msgs, trailing) =
            crate::tcpmux::call_streaming_tcp_mux(channel, transport, method, metadata, requests)?;
        return Ok((first_or_empty(msgs), trailing));
    }
    #[cfg(feature = "transport-h3")]
    if channel.h3.is_some() {
        return crate::transport_h3::call_client_streaming_h3(
            channel, transport, method, metadata, requests,
        );
    }
    #[cfg(feature = "transport-ws-mux")]
    if channel.wsmux.is_some() {
        let (msgs, trailing) =
            crate::wsmux::call_streaming_ws_mux(channel, transport, method, metadata, requests)?;
        return Ok((first_or_empty(msgs), trailing));
    }
    Err(RpcError::TransportUnavailable)
}

/// Bidirectional-streaming RPC, blocking batch shape:
/// every request out, every response collected.
/// Dispatches on the channel's negotiated transport.
pub fn call_bidi(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    #[cfg(feature = "transport-h2")]
    if channel.h2.is_some() {
        return crate::transport_h2::call_streaming_h2(
            channel, transport, method, metadata, requests,
        );
    }
    #[cfg(feature = "transport-tcp-mux")]
    if channel.tcpmux.is_some() {
        return crate::tcpmux::call_streaming_tcp_mux(
            channel, transport, method, metadata, requests,
        );
    }
    #[cfg(feature = "transport-h3")]
    if channel.h3.is_some() {
        return crate::transport_h3::call_bidi_h3(channel, transport, method, metadata, requests);
    }
    #[cfg(feature = "transport-ws-mux")]
    if channel.wsmux.is_some() {
        return crate::wsmux::call_streaming_ws_mux(channel, transport, method, metadata, requests);
    }
    Err(RpcError::TransportUnavailable)
}

// ============================================================
// Connected channel (D5 — the tonic-shaped bootstrap:
// docs/dx-scope.md).
// ============================================================

/// A [`Channel`](crate::Channel) bundled with its
/// transport — the tonic-habit client handle. Generated
/// client fns take `&mut ConnectedChannel`; the blocking
/// drive happens inside.
pub struct ConnectedChannel {
    pub(crate) channel: crate::Channel,
    pub(crate) transport: Box<dyn DatagramTransport>,
}

impl ConnectedChannel {
    /// Bundle an existing sans-IO channel with any
    /// transport (in-memory pairs in tests, sockets in
    /// production).
    pub fn new(channel: crate::Channel, transport: Box<dyn DatagramTransport>) -> Self {
        ConnectedChannel { channel, transport }
    }

    /// The underlying sans-IO channel — for per-channel
    /// tuning ([`Channel::set_link_liveness`](crate::Channel::set_link_liveness),
    /// [`Channel::set_datagram_budget`](crate::Channel::set_datagram_budget)).
    pub fn channel_mut(&mut self) -> &mut crate::Channel {
        &mut self.channel
    }

    /// Unary RPC, blocking.
    pub fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        call_unary(
            &mut self.channel,
            self.transport.as_mut(),
            method,
            metadata,
            request,
        )
    }

    /// Server-streaming RPC, blocking.
    pub fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        call_server_streaming(
            &mut self.channel,
            self.transport.as_mut(),
            method,
            metadata,
            request,
        )
    }

    /// Client-streaming RPC, blocking.
    pub fn call_client_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        call_client_streaming(
            &mut self.channel,
            self.transport.as_mut(),
            method,
            metadata,
            requests,
        )
    }

    /// Bidirectional-streaming RPC, blocking batch shape.
    pub fn call_bidi(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        call_bidi(
            &mut self.channel,
            self.transport.as_mut(),
            method,
            metadata,
            requests,
        )
    }
}

impl crate::RpcChannel for ConnectedChannel {
    fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        ConnectedChannel::call_unary(self, method, metadata, request)
    }

    fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        ConnectedChannel::call_server_streaming(self, method, metadata, request)
    }

    fn call_client_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        ConnectedChannel::call_client_streaming(self, method, metadata, requests)
    }

    fn call_bidi(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        ConnectedChannel::call_bidi(self, method, metadata, requests)
    }
}

// ============================================================
// TCP transport (byte-stream chunks through the same
// DatagramTransport seam — the pumps are chunk-oriented,
// TCP boundaries don't matter). Shared by h2c and
// tcp-mux.
// ============================================================

/// Non-blocking TCP transport.
pub struct TcpTransport {
    stream: std::net::TcpStream,
    buf: Box<[u8; 65536]>,
    /// Reactor handle for the idle park; built on first
    /// [`DatagramTransport::wait_readable`].
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    parker: Option<FdParker>,
}

impl TcpTransport {
    /// Connect to `addr`.
    pub fn connect(addr: &str) -> Result<Self, RpcError> {
        let stream = std::net::TcpStream::connect(addr)
            .map_err(|_| RpcError::Transport("tcp: connect failed"))?;
        Self::from_stream(stream)
    }

    /// Wrap an accepted stream (server side).
    pub fn from_stream(stream: std::net::TcpStream) -> Result<Self, RpcError> {
        stream
            .set_nonblocking(true)
            .map_err(|_| RpcError::Transport("tcp: nonblocking failed"))?;
        stream.set_nodelay(true).ok();
        Ok(TcpTransport {
            stream,
            buf: Box::new([0u8; 65536]),
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd"
            ))]
            parker: None,
        })
    }
}

impl DatagramTransport for TcpTransport {
    fn send(&mut self, chunk: &[u8]) -> Result<(), RpcError> {
        use std::io::Write;
        // Non-blocking write loop: a chunk larger than the
        // free kernel send buffer hits WouldBlock mid-way
        // (`write_all` would abort and corrupt the
        // stream). Spin-yield until the peer drains —
        // request-response shaped traffic guarantees a
        // reader on the other side — with a stall cap for
        // dead peers that stopped reading.
        let mut off = 0;
        let mut stalls = 0u32;
        while off < chunk.len() {
            match self.stream.write(&chunk[off..]) {
                Ok(0) => return Err(RpcError::Transport("tcp: peer closed")),
                Ok(n) => {
                    off += n;
                    stalls = 0;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stalls >= 4_000_000 {
                        return Err(RpcError::Transport("tcp: send stalled"));
                    }
                    stalls += 1;
                    std::thread::yield_now();
                }
                Err(_) => return Err(RpcError::Transport("tcp: write failed")),
            }
        }
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        use std::io::Read;
        match self.stream.read(&mut self.buf[..]) {
            Ok(0) => Err(RpcError::Transport("tcp: peer closed")),
            Ok(n) => Ok(Some(self.buf[..n].to_vec())),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(_) => Err(RpcError::Transport("tcp: read failed")),
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    fn wait_readable(&mut self, timeout: core::time::Duration) -> Result<bool, RpcError> {
        use std::os::fd::AsRawFd;
        if self.parker.is_none() {
            self.parker = Some(FdParker::new(self.stream.as_raw_fd())?);
        }
        if let Some(p) = self.parker.as_mut() {
            p.park(timeout)?;
        }
        Ok(true)
    }
}

// ============================================================
// Shared TCP accept loop (h2c / tcp-mux servers).
// ============================================================

/// What the accept loop needs from one server-side
/// connection: bytes in/out + request dispatch.
#[cfg(any(
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
pub(crate) trait TcpServeConn {
    fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError>;
    fn dispatch(&mut self, server: &crate::Server) -> Result<usize, RpcError>;
}

/// [`serve_tcp_until_with`] over a bare [`TcpTransport`]
/// (the h2c / tcp-mux shape).
#[cfg(any(
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
pub(crate) fn serve_tcp_until<C: TcpServeConn + Send + 'static>(
    server: &crate::Server,
    addr: &str,
    stop: &core::sync::atomic::AtomicBool,
    accept: impl Fn(&crate::Server) -> C,
) -> Result<(), RpcError> {
    serve_tcp_until_with(server, addr, stop, accept, TcpTransport::from_stream)
}

/// One connection's serve task: drain → pump → dispatch →
/// flush, then a short spin and an event-driven wait
/// (`readable` on the conn socket, sliced by a 5 ms
/// timeout — the slice bounds only stop latency; traffic
/// wakes the task immediately via the reactor). Peer errors end the
/// task — a client hanging up is lifecycle, not failure —
/// and a poisoned connection no longer takes the whole
/// server down with it (pre-0.16 the single-thread loop
/// `?`-propagated per-conn pump errors).
#[cfg(any(
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
async fn drive_tcp_conn<C: TcpServeConn, T: DatagramTransport>(
    mut t: T,
    mut conn: C,
    fd: grpcx_io::RawFd,
    server: crate::Server,
    stop: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
) {
    use core::sync::atomic::Ordering;
    let mut idle = 0u32;
    // Passive idle GC (0.25): reap once no inbound bytes
    // arrive within the server's max_connection_idle.
    let max_idle = server.max_connection_idle();
    let mut last_inbound = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if let Some(budget) = max_idle
            && last_inbound.elapsed() > budget
        {
            return; // reaped — task exits, conn drops
        }
        let mut progressed = false;
        loop {
            match t.recv() {
                Ok(Some(chunk)) => {
                    progressed = true;
                    last_inbound = std::time::Instant::now();
                    let Ok(outs) = conn.pump(Some(&chunk)) else {
                        return;
                    };
                    for out in outs {
                        if t.send(&out).is_err() {
                            return;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        if conn.dispatch(&server).is_err() {
            return;
        }
        let Ok(outs) = conn.pump(None) else {
            return;
        };
        for out in outs {
            progressed = true;
            if t.send(&out).is_err() {
                return;
            }
        }
        if progressed {
            idle = 0;
        } else if idle < 4 {
            // The drive_once spin budget (measured there):
            // catch chunks already in flight without a
            // reactor round trip.
            idle += 1;
            grpcx_rt::yield_now().await;
        } else {
            let _ = grpcx_rt::timeout(core::time::Duration::from_millis(5), grpcx_rt::readable(fd))
                .await;
        }
    }
}

/// Bind a TCP listener and serve until `stop` flips —
/// event-driven and multi-core (the L2-Runtime serve,
/// charter 2026-06-12). The accept loop stays on the
/// calling thread (parking on the LISTENER fd in
/// [`PARK_SLICE`] slices); each accepted connection runs
/// as a task on a work-stealing [`grpcx_rt::Runtime`],
/// woken by `readable` on its own socket. `wrap` builds
/// the per-connection transport from the accepted stream
/// (bare TCP, or the TLS middleware); the connection fd is
/// taken from the stream BEFORE wrapping, so the
/// [`DatagramTransport`] seam needs no fd surface.
#[cfg(any(
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
pub(crate) fn serve_tcp_until_with<C, T>(
    server: &crate::Server,
    addr: &str,
    stop: &core::sync::atomic::AtomicBool,
    accept: impl Fn(&crate::Server) -> C,
    wrap: impl Fn(std::net::TcpStream) -> Result<T, RpcError>,
) -> Result<(), RpcError>
where
    C: TcpServeConn + Send + 'static,
    T: DatagramTransport + Send + 'static,
{
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::os::fd::AsRawFd;
    let listener =
        std::net::TcpListener::bind(addr).map_err(|_| RpcError::Transport("tcp: bind failed"))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| RpcError::Transport("tcp: nonblocking failed"))?;

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    let rt = grpcx_rt::Runtime::new(workers)
        .map_err(|_| RpcError::Transport("rt: runtime init failed"))?;
    // Tasks can't borrow the caller's `stop` (`spawn` needs
    // 'static) — they watch this owned mirror instead.
    let task_stop = alloc::sync::Arc::new(AtomicBool::new(false));
    let live = alloc::sync::Arc::new(AtomicUsize::new(0));

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    let mut parker: Option<FdParker> = None;

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let fd = stream.as_raw_fd();
                let t = wrap(stream)?;
                let conn = accept(server);
                let server = server.clone();
                let task_stop = alloc::sync::Arc::clone(&task_stop);
                let live = alloc::sync::Arc::clone(&live);
                live.fetch_add(1, Ordering::AcqRel);
                rt.spawn(async move {
                    drive_tcp_conn(t, conn, fd, server, task_stop).await;
                    live.fetch_sub(1, Ordering::AcqRel);
                });
                // Re-poll immediately — accepts arrive in
                // bursts.
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                task_stop.store(true, Ordering::Relaxed);
                return Err(RpcError::Transport("tcp: accept failed"));
            }
        }
        // Quiet listener: park until a connection attempt
        // lands (or one stall slice — bounds stop latency).
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd"
        ))]
        {
            if parker.is_none() {
                parker = Some(FdParker::new(listener.as_raw_fd())?);
            }
            if let Some(p) = parker.as_mut() {
                p.park(PARK_SLICE)?;
            }
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd"
        )))]
        std::thread::yield_now();
    }

    // Wind down: connection tasks observe the mirror within
    // one ~5 ms slice. The cap is a dead-man guard, not an
    // expected path; dropping the Runtime joins its workers
    // either way.
    task_stop.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + core::time::Duration::from_secs(2);
    while live.load(Ordering::Acquire) > 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    Ok(())
}
