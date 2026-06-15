// `grpcx/h3` transport wiring — v0.3 (work unit 3).
//
// Sans-IO at the core: `Channel` / `ServerConn` own an L5
// `grpcx_h3::Connection` and expose the same `pump`
// (datagram in → datagrams out) surface as L4/L5, so the
// runtime stays pluggable. The blocking `call_unary`
// facade drives a [`DatagramTransport`] (the IO seam —
// in-memory queues in tests, a UDP socket once the L1/L2
// runtime is wired at v0.6+).
//
// gRPC shapes handled:
// - request:  HEADERS + DATA(length-prefixed frame) + FIN
// - response: HEADERS(:status 200) + DATA(frame) +
//             trailing HEADERS(grpc-status …) + FIN
// - trailers-only response (error fast path): a single
//   HEADERS section carrying `grpc-status`, no body.

use crate::transport_common::{ConnectedChannel, DatagramTransport, drive_once};
use crate::{
    Metadata, Method, RpcError, Server, Status, StatusCode, TransportKind, decode_all_frames,
    encode_request_headers, encode_response_headers, encode_trailers, parse_request_headers,
    parse_trailers, try_decode_frame,
};
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use grpcx_crypto::TlsConfig;
use grpcx_h3::{Connection as H3Connection, Header, Settings};
use grpcx_quic::QuicConnection;

// ============================================================
// Error mapping.
// ============================================================

fn map_h3(e: grpcx_h3::Error) -> RpcError {
    RpcError::Transport(match e {
        grpcx_h3::Error::BadFrame => "h3: malformed frame",
        grpcx_h3::Error::QpackDecode => "h3: qpack decode",
        grpcx_h3::Error::QpackEncode => "h3: qpack encode",
        grpcx_h3::Error::Quic(grpcx_quic::Error::Timeout) => "h3: link dead",
        grpcx_h3::Error::Quic(_) => "h3: quic transport error",
        grpcx_h3::Error::Internal => "h3: internal",
    })
}

fn to_h3_headers(meta: &Metadata) -> Vec<Header> {
    meta.headers
        .iter()
        .map(|(n, v)| Header {
            name: n.as_bytes().to_vec(),
            value: v.clone(),
        })
        .collect()
}

/// Write the whole buffer or error: L5's `write_data`
/// returns the accepted byte count (QUIC flow control can
/// take less than offered). Silently dropping the tail
/// would corrupt the gRPC frame stream, so a short write
/// is a hard transport error until v1.x grows buffered
/// retry on top of flow-control credit.
fn write_all(
    conn: &mut H3Connection,
    rs: &grpcx_h3::RequestStream,
    wire: &[u8],
) -> Result<(), RpcError> {
    let n = conn.write_data(rs, wire).map_err(map_h3)?;
    if n != wire.len() {
        return Err(RpcError::Transport("partial write: send window full"));
    }
    Ok(())
}

/// Send one gRPC message as its 5-byte length-prefixed
/// frame: header and payload go down as two writes, so the
/// payload is never staged in an intermediate `Vec` (the
/// old `encode_frame` round-trip copied the whole message
/// once more — hot at 64 KiB). On the wire this lands as
/// two H/3 DATA frames; RFC 9114 allows any segmentation
/// and the receive side reassembles a contiguous body.
fn write_message_frame(
    conn: &mut H3Connection,
    rs: &grpcx_h3::RequestStream,
    payload: &[u8],
) -> Result<(), RpcError> {
    let len = u32::try_from(payload.len())
        .expect("gRPC frame payload exceeds u32::MAX (4 GiB) — unrepresentable on the wire");
    let mut hdr = [0u8; 5];
    hdr[1..].copy_from_slice(&len.to_be_bytes());
    write_all(conn, rs, &hdr)?;
    write_all(conn, rs, payload)
}

fn from_h3_headers(hdrs: &[Header]) -> Result<Metadata, RpcError> {
    let mut headers = Vec::with_capacity(hdrs.len());
    for h in hdrs {
        let name =
            core::str::from_utf8(&h.name).map_err(|_| RpcError::Codec("non-UTF-8 header name"))?;
        headers.push((name.to_string(), h.value.clone()));
    }
    Ok(Metadata { headers })
}

// ============================================================
// Client side.
// ============================================================

pub(crate) struct H3ClientInner {
    conn: H3Connection,
    authority: String,
    /// Per-connection monotonic epoch — every pump is
    /// stamped `epoch.elapsed()` µs so L4's sans-IO timers
    /// (RTT, PTO/tail-loss, handshake retransmit) run.
    epoch: std::time::Instant,
}

impl H3ClientInner {
    pub(crate) fn is_ready(&self) -> bool {
        self.conn.is_ready()
    }
}

/// The H/3 arm of [`Channel::pump`](crate::Channel::pump).
pub(crate) fn pump_h3(
    channel: &mut crate::Channel,
    incoming: Option<&[u8]>,
) -> Result<Vec<Vec<u8>>, RpcError> {
    let inner = channel.h3_inner()?;
    let now_us = inner.epoch.elapsed().as_micros() as u64;
    inner.conn.pump_at(incoming, now_us).map_err(map_h3)
}

/// In-flight unary call state — created by
/// [`Channel::begin_unary`], advanced by
/// [`Channel::poll_unary`].
///
/// [`Channel::begin_unary`]: crate::Channel::begin_unary
/// [`Channel::poll_unary`]: crate::Channel::poll_unary
pub struct UnaryCall {
    stream: grpcx_h3::RequestStream,
    leading: Option<Metadata>,
    body: Vec<u8>,
}

impl crate::Channel {
    /// Open a sans-IO `grpcx/h3` channel: constructs the
    /// L4 QUIC client + L5 H/3 session. The QUIC/TLS
    /// handshake completes through [`pump`](Self::pump)
    /// cycles; [`is_ready`](Self::is_ready) reports
    /// readiness. `authority` lands in the `:authority`
    /// pseudo-header of every call.
    pub fn connect_h3(
        tls: &TlsConfig,
        server_name: &str,
        authority: &str,
    ) -> Result<Self, RpcError> {
        let quic = QuicConnection::new_client(tls, server_name, b"grpcx-c".to_vec())
            .map_err(|_| RpcError::Transport("quic client construction failed"))?;
        let conn = H3Connection::new_client(quic, Settings::default()).map_err(map_h3)?;
        Ok(crate::Channel {
            transport: TransportKind::H3,
            h3: Some(H3ClientInner {
                conn,
                authority: authority.to_string(),
                epoch: std::time::Instant::now(),
            }),
            #[cfg(feature = "transport-ws-mux")]
            wsmux: None,
            #[cfg(feature = "transport-h2")]
            h2: None,
            #[cfg(feature = "transport-tcp-mux")]
            tcpmux: None,
        })
    }

    fn h3_inner(&mut self) -> Result<&mut H3ClientInner, RpcError> {
        self.h3.as_mut().ok_or(RpcError::TransportUnavailable)
    }

    /// Set the QUIC 1-RTT plaintext budget per datagram
    /// (H/3 channels only). Datagram size ≈ budget + ~30 B;
    /// the caller asserts it fits the path MTU — the 1350
    /// default is internet-safe, loopback links can go to
    /// ~9 000 (Darwin's `net.inet.udp.maxdgram`). Fewer,
    /// larger datagrams amortize per-packet sealing, ACKs,
    /// and scheduling on bulk transfers.
    pub fn set_datagram_budget(&mut self, plaintext_budget: usize) -> Result<(), RpcError> {
        self.h3_inner()?.conn.set_datagram_budget(plaintext_budget);
        Ok(())
    }

    pub(crate) fn set_link_liveness_h3(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) -> Result<(), RpcError> {
        self.h3_inner()?.conn.set_link_liveness_us(
            keepalive.as_micros() as u64,
            idle_timeout.as_micros() as u64,
        );
        Ok(())
    }

    /// Start a unary call: sends request HEADERS + the
    /// length-prefixed request frame + FIN. Drive with
    /// [`poll_unary`](Self::poll_unary) between
    /// [`pump`](Self::pump) cycles.
    pub fn begin_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<UnaryCall, RpcError> {
        let inner = self.h3_inner()?;
        let authority = inner.authority.clone();
        let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
        let stream = inner
            .conn
            .send_request(&to_h3_headers(&hdrs))
            .map_err(map_h3)?;
        write_message_frame(&mut inner.conn, &stream, request)?;
        inner.conn.finish(&stream).map_err(map_h3)?;
        Ok(UnaryCall {
            stream,
            leading: None,
            body: Vec::new(),
        })
    }

    /// Advance a unary call. `Ok(None)` = still pending —
    /// keep pumping. `Ok(Some((payload, trailing_meta)))`
    /// = the peer answered `grpc-status: 0` with one
    /// response message. RPC failures surface as
    /// [`RpcError::Status`].
    pub fn poll_unary(
        &mut self,
        call: &mut UnaryCall,
    ) -> Result<Option<(Vec<u8>, Metadata)>, RpcError> {
        let inner = self.h3_inner()?;

        if call.leading.is_none() {
            let Some(hdrs) = inner.conn.read_headers(&call.stream).map_err(map_h3)? else {
                return Ok(None);
            };
            let meta = from_h3_headers(&hdrs)?;
            // Trailers-only response (gRPC error fast
            // path): the single HEADERS section carries
            // grpc-status directly.
            if meta.headers.iter().any(|(n, _)| n == "grpc-status") {
                let (status, trailing) = parse_trailers(&meta)?;
                inner.conn.close_request(&call.stream);
                return if status.code == StatusCode::Ok {
                    Ok(Some((Vec::new(), trailing)))
                } else {
                    Err(RpcError::Status(status))
                };
            }
            let ok_status = meta
                .headers
                .iter()
                .any(|(n, v)| n == ":status" && v == b"200");
            if !ok_status {
                return Err(RpcError::Transport("h3: non-200 response"));
            }
            call.leading = Some(meta);
        }

        let (_, _fin) = inner
            .conn
            .read_data(&call.stream, &mut call.body)
            .map_err(map_h3)?;

        if !inner.conn.received_trailers_ready(&call.stream) {
            return Ok(None);
        }
        let trailers = inner
            .conn
            .read_trailers(&call.stream)
            .map_err(map_h3)?
            .expect("ready implies Some");
        let (status, trailing) = parse_trailers(&from_h3_headers(&trailers)?)?;
        inner.conn.close_request(&call.stream);
        if status.code != StatusCode::Ok {
            return Err(RpcError::Status(status));
        }

        let frames = decode_all_frames(&call.body)?;
        let payload = match frames.into_iter().next() {
            Some(f) if !f.compressed => f.payload,
            Some(_) => return Err(RpcError::Codec("unexpected compressed response")),
            None => Vec::new(),
        };
        Ok(Some((payload, trailing)))
    }
}

// ============================================================
// Client-side streaming calls (v0.4).
// ============================================================

/// One event from a server-streaming response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// One response message arrived.
    Message(Vec<u8>),
    /// The stream completed with `grpc-status: 0`; carries
    /// the trailing metadata.
    Complete(Metadata),
}

/// In-flight server-streaming call — created by
/// [`Channel::begin_server_streaming`], advanced by
/// [`Channel::poll_stream_message`].
///
/// [`Channel::begin_server_streaming`]: crate::Channel::begin_server_streaming
/// [`Channel::poll_stream_message`]: crate::Channel::poll_stream_message
pub struct ServerStreamingCall {
    stream: grpcx_h3::RequestStream,
    leading: Option<Metadata>,
    buf: Vec<u8>,
    pos: usize,
}

/// In-flight client-streaming call: send many messages,
/// then [`Channel::finish_sending`] and poll for the
/// single response — which is exactly the unary
/// wait-for-one-response shape, so the state is shared.
///
/// [`Channel::finish_sending`]: crate::Channel::finish_sending
pub type ClientStreamingCall = UnaryCall;

impl crate::Channel {
    /// Start a server-streaming call: one request message
    /// out (FIN), a stream of responses back. Drive with
    /// [`poll_stream_message`](Self::poll_stream_message)
    /// between [`pump`](Self::pump) cycles.
    pub fn begin_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<ServerStreamingCall, RpcError> {
        let call = self.begin_unary(method, metadata, request)?;
        Ok(ServerStreamingCall {
            stream: call.stream,
            leading: None,
            buf: Vec::new(),
            pos: 0,
        })
    }

    /// Advance a server-streaming call. `Ok(None)` = still
    /// pending; `Ok(Some(StreamEvent::Message(m)))` = one
    /// response message; `Ok(Some(StreamEvent::Complete))`
    /// = the peer closed the stream with `grpc-status: 0`.
    /// RPC failures surface as [`RpcError::Status`].
    pub fn poll_stream_message(
        &mut self,
        call: &mut ServerStreamingCall,
    ) -> Result<Option<StreamEvent>, RpcError> {
        let inner = self.h3_inner()?;

        if call.leading.is_none() {
            let Some(hdrs) = inner.conn.read_headers(&call.stream).map_err(map_h3)? else {
                return Ok(None);
            };
            let meta = from_h3_headers(&hdrs)?;
            // Trailers-only response (error fast path /
            // empty-stream completion).
            if meta.headers.iter().any(|(n, _)| n == "grpc-status") {
                let (status, trailing) = parse_trailers(&meta)?;
                inner.conn.close_request(&call.stream);
                return if status.code == StatusCode::Ok {
                    Ok(Some(StreamEvent::Complete(trailing)))
                } else {
                    Err(RpcError::Status(status))
                };
            }
            let ok_status = meta
                .headers
                .iter()
                .any(|(n, v)| n == ":status" && v == b"200");
            if !ok_status {
                return Err(RpcError::Transport("h3: non-200 response"));
            }
            call.leading = Some(meta);
        }

        // Accumulate body bytes, then yield the next
        // complete frame if one is buffered.
        let _ = inner
            .conn
            .read_data(&call.stream, &mut call.buf)
            .map_err(map_h3)?;
        if let Some((frame, consumed)) = try_decode_frame(&call.buf[call.pos..])? {
            call.pos += consumed;
            if frame.compressed {
                return Err(RpcError::Codec("unexpected compressed response"));
            }
            return Ok(Some(StreamEvent::Message(frame.payload)));
        }

        if !inner.conn.received_trailers_ready(&call.stream) {
            return Ok(None);
        }
        let trailers = inner
            .conn
            .read_trailers(&call.stream)
            .map_err(map_h3)?
            .expect("ready implies Some");
        let (status, trailing) = parse_trailers(&from_h3_headers(&trailers)?)?;
        inner.conn.close_request(&call.stream);
        if status.code != StatusCode::Ok {
            return Err(RpcError::Status(status));
        }
        if call.pos != call.buf.len() {
            return Err(RpcError::Codec("truncated trailing frame in stream"));
        }
        Ok(Some(StreamEvent::Complete(trailing)))
    }

    /// Start a client-streaming call: request HEADERS go
    /// out, the stream stays open for
    /// [`send_stream_message`](Self::send_stream_message)
    /// until [`finish_sending`](Self::finish_sending).
    pub fn begin_client_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
    ) -> Result<ClientStreamingCall, RpcError> {
        let inner = self.h3_inner()?;
        let authority = inner.authority.clone();
        let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
        let stream = inner
            .conn
            .send_request(&to_h3_headers(&hdrs))
            .map_err(map_h3)?;
        Ok(UnaryCall {
            stream,
            leading: None,
            body: Vec::new(),
        })
    }

    /// Send one request message on an open
    /// client-streaming call.
    pub fn send_stream_message(
        &mut self,
        call: &ClientStreamingCall,
        message: &[u8],
    ) -> Result<(), RpcError> {
        let stream = call.stream;
        self.send_message_on(&stream, message)
    }

    /// Close the request direction of a client-streaming
    /// call. The single response is then collected with
    /// [`poll_unary`](Self::poll_unary).
    pub fn finish_sending(&mut self, call: &ClientStreamingCall) -> Result<(), RpcError> {
        self.h3_inner()?.conn.finish(&call.stream).map_err(map_h3)
    }

    /// Start a bidirectional-streaming call: request
    /// HEADERS go out and the stream stays open in both
    /// directions. Send with
    /// [`send_bidi_message`](Self::send_bidi_message) /
    /// [`finish_bidi_sending`](Self::finish_bidi_sending);
    /// receive with
    /// [`poll_stream_message`](Self::poll_stream_message)
    /// (a [`BidiCall`] IS a [`ServerStreamingCall`] on the
    /// receive side).
    pub fn begin_bidi(
        &mut self,
        method: &Method,
        metadata: &Metadata,
    ) -> Result<BidiCall, RpcError> {
        let inner = self.h3_inner()?;
        let authority = inner.authority.clone();
        let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
        let stream = inner
            .conn
            .send_request(&to_h3_headers(&hdrs))
            .map_err(map_h3)?;
        Ok(ServerStreamingCall {
            stream,
            leading: None,
            buf: Vec::new(),
            pos: 0,
        })
    }

    /// Send one request message on an open bidi call.
    pub fn send_bidi_message(&mut self, call: &BidiCall, message: &[u8]) -> Result<(), RpcError> {
        let stream = call.stream;
        self.send_message_on(&stream, message)
    }

    /// Close the request direction of a bidi call;
    /// responses keep flowing via
    /// [`poll_stream_message`](Self::poll_stream_message).
    pub fn finish_bidi_sending(&mut self, call: &BidiCall) -> Result<(), RpcError> {
        self.h3_inner()?.conn.finish(&call.stream).map_err(map_h3)
    }

    /// Shared frame-and-send for the streaming send paths.
    fn send_message_on(
        &mut self,
        stream: &grpcx_h3::RequestStream,
        message: &[u8],
    ) -> Result<(), RpcError> {
        let inner = self.h3_inner()?;
        write_message_frame(&mut inner.conn, stream, message)
    }
}

/// In-flight bidirectional-streaming call — the receive
/// side is exactly the server-streaming shape, so the
/// state is shared. Created by
/// [`Channel::begin_bidi`](crate::Channel::begin_bidi).
pub type BidiCall = ServerStreamingCall;

impl crate::Channel {
    /// Eager cancel (0.22): abandon an in-flight h3 call —
    /// QUIC RESET_STREAM + STOP_SENDING queued; flushed by
    /// the caller's next pump.
    pub(crate) fn cancel_h3_call(&mut self, rs: &grpcx_h3::RequestStream) {
        if let Some(inner) = self.h3.as_mut() {
            inner.conn.cancel_request(rs);
        }
    }
}

/// Deadline trip with an in-flight call: cancel the stream
/// on the wire (best effort — one pump flushes the
/// RESET_STREAM/STOP_SENDING pair, errors ignored: the
/// verdict must come back regardless), then surface the
/// DEADLINE_EXCEEDED error.
fn deadline_check_cancelling(
    deadline: &crate::transport_common::CallDeadline,
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    rs: &grpcx_h3::RequestStream,
) -> Result<(), RpcError> {
    let Err(e) = deadline.check() else {
        return Ok(());
    };
    channel.cancel_h3_call(rs);
    if let Ok(outs) = channel.pump(None) {
        for out in outs {
            let _ = transport.send(&out);
        }
    }
    Err(e)
}

/// The H/3 arm of the blocking unary facade — see
/// [`call_unary`](crate::call_unary) for the dispatching
/// entry point.
pub(crate) fn call_unary_h3(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    // Drive the handshake first so begin_unary opens its
    // stream on a ready session. The caller's deadline
    // covers establishment too (gRPC semantics).
    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    while !channel.is_ready() {
        deadline.check()?;
        idle = drive_once(channel, transport, idle)?;
    }
    let mut call = channel.begin_unary(method, metadata, request)?;
    let mut idle = 0u32;
    loop {
        deadline_check_cancelling(&deadline, channel, transport, &call.stream)?;
        if let Some(done) = channel.poll_unary(&mut call)? {
            return Ok(done);
        }
        idle = drive_once(channel, transport, idle)?;
    }
}

/// Server-streaming RPC, blocking: one request out, every
/// response message collected until the stream completes.
pub(crate) fn call_server_streaming_h3(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    while !channel.is_ready() {
        deadline.check()?;
        idle = drive_once(channel, transport, idle)?;
    }
    let mut call = channel.begin_server_streaming(method, metadata, request)?;
    let mut out = Vec::new();
    let mut idle = 0u32;
    loop {
        deadline_check_cancelling(&deadline, channel, transport, &call.stream)?;
        match channel.poll_stream_message(&mut call)? {
            Some(StreamEvent::Message(m)) => {
                out.push(m);
                idle = 0;
            }
            Some(StreamEvent::Complete(trailing)) => return Ok((out, trailing)),
            None => idle = drive_once(channel, transport, idle)?,
        }
    }
}

/// Client-streaming RPC, blocking: every request message
/// out, then the single response.
pub(crate) fn call_client_streaming_h3(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    while !channel.is_ready() {
        deadline.check()?;
        idle = drive_once(channel, transport, idle)?;
    }
    let mut call = channel.begin_client_streaming(method, metadata)?;
    for r in requests {
        channel.send_stream_message(&call, r)?;
    }
    channel.finish_sending(&call)?;
    let mut idle = 0u32;
    loop {
        deadline_check_cancelling(&deadline, channel, transport, &call.stream)?;
        if let Some(done) = channel.poll_unary(&mut call)? {
            return Ok(done);
        }
        idle = drive_once(channel, transport, idle)?;
    }
}

/// Bidirectional-streaming RPC, blocking batch shape:
/// every request out, every response collected.
pub(crate) fn call_bidi_h3(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    while !channel.is_ready() {
        deadline.check()?;
        idle = drive_once(channel, transport, idle)?;
    }
    let call = channel.begin_bidi(method, metadata)?;
    for r in requests {
        channel.send_bidi_message(&call, r)?;
    }
    channel.finish_bidi_sending(&call)?;
    let mut call = call;
    let mut out = Vec::new();
    let mut idle = 0u32;
    loop {
        deadline_check_cancelling(&deadline, channel, transport, &call.stream)?;
        match channel.poll_stream_message(&mut call)? {
            Some(StreamEvent::Message(m)) => {
                out.push(m);
                idle = 0;
            }
            Some(StreamEvent::Complete(trailing)) => return Ok((out, trailing)),
            None => idle = drive_once(channel, transport, idle)?,
        }
    }
}

// ============================================================
// UDP-socket transport (std).
// ============================================================

/// [`DatagramTransport`] over a real OS UDP socket
/// (non-blocking). Sends and receives ride the Darwin
/// batch syscalls where available; a quiet link parks in
/// the L1 reactor
/// ([`wait_readable`](DatagramTransport::wait_readable))
/// instead of spinning. The remaining L2 integration —
/// driving many connections on the multithread `Runtime` —
/// is the v1.x runtime pass.
pub struct UdpTransport {
    sock: std::net::UdpSocket,
    peer: core::net::SocketAddr,
    buf: Box<[u8; 65535]>,
    /// `true` once the socket is OS-connected to `peer` —
    /// unlocks the batch syscalls (Darwin `sendmsg_x` /
    /// `recvmsg_x` reject unconnected sockets; Linux
    /// `sendmmsg` rides the connected peer the same way).
    connected: bool,
    /// Reused receive slots for the batch syscall (heap —
    /// `UDP_BATCH x UDP_BATCH_SLOT` is too big to stack
    /// per call, and re-zeroing it per call was pure
    /// waste).
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    batch_buf: Box<[u8]>,
    /// Reactor handle for the idle park; built on first
    /// [`DatagramTransport::wait_readable`].
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    parker: Option<crate::transport_common::FdParker>,
}

/// Per-call batch width for the batch syscalls.
/// 8 covers a full 64 KiB burst at the default-sysctl
/// budget (9 000 ⇒ 8 datagrams) in one syscall.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
const UDP_BATCH: usize = 8;
/// Per-slot receive buffer — the largest datagram UDP can
/// carry at all (u16 length), so a raised
/// `net.inet.udp.maxdgram` sysctl (default 9 216) never
/// truncates a batch receive. 8 × 64 KiB = the same 512 KiB
/// footprint as the previous 32 × 16 KiB layout.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
const UDP_BATCH_SLOT: usize = 65535;

impl UdpTransport {
    /// Bind `local` (e.g. `"127.0.0.1:0"`) and aim at
    /// `peer`. The socket is set non-blocking —
    /// [`recv`](DatagramTransport::recv) returns
    /// `Ok(None)` when no datagram is queued.
    pub fn bind(local: &str, peer: &str) -> Result<Self, RpcError> {
        let sock = std::net::UdpSocket::bind(local)
            .map_err(|_| RpcError::Transport("udp: bind failed"))?;
        sock.set_nonblocking(true)
            .map_err(|_| RpcError::Transport("udp: nonblocking failed"))?;
        let peer = peer
            .parse()
            .map_err(|_| RpcError::Transport("udp: bad peer address"))?;
        Ok(UdpTransport {
            sock,
            peer,
            buf: Box::new([0u8; 65535]),
            connected: false,
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
            batch_buf: vec![0u8; UDP_BATCH * UDP_BATCH_SLOT].into_boxed_slice(),
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd"
            ))]
            parker: None,
        })
    }

    /// The locally-bound address (port resolution for
    /// `"…:0"` binds).
    pub fn local_addr(&self) -> Result<core::net::SocketAddr, RpcError> {
        self.sock
            .local_addr()
            .map_err(|_| RpcError::Transport("udp: local_addr failed"))
    }

    /// Re-aim at a new peer (server side: lock onto the
    /// first client seen).
    pub fn set_peer(&mut self, peer: core::net::SocketAddr) {
        self.peer = peer;
        self.connected = false;
    }

    /// OS-connect to the current peer once it's known to
    /// be real (the client's server addr, or the first
    /// sender a server-side transport locked onto). UDP
    /// connect sends nothing — it just fixes the
    /// destination and unlocks the batch syscalls.
    fn upgrade_to_connected(&mut self) {
        if !self.connected && self.sock.connect(self.peer).is_ok() {
            self.connected = true;
        }
    }
}

impl DatagramTransport for UdpTransport {
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError> {
        let r = if self.connected {
            self.sock.send(datagram)
        } else {
            self.sock.send_to(datagram, self.peer)
        };
        r.map_err(|_| RpcError::Transport("udp: send failed"))?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        if self.connected {
            return match self.sock.recv(&mut self.buf[..]) {
                Ok(n) => Ok(Some(self.buf[..n].to_vec())),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(_) => Err(RpcError::Transport("udp: recv failed")),
            };
        }
        match self.sock.recv_from(&mut self.buf[..]) {
            Ok((n, from)) => {
                // Lock onto the sender — lets the server
                // side answer whoever spoke first — and
                // OS-connect to it (a real peer by
                // definition: it just sent us a datagram).
                self.peer = from;
                self.upgrade_to_connected();
                Ok(Some(self.buf[..n].to_vec()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                Err(RpcError::Transport(crate::transport_common::UDP_REFUSED))
            }
            Err(_) => Err(RpcError::Transport("udp: recv failed")),
        }
    }

    /// Darwin `sendmsg_x` / Linux `sendmmsg`: one syscall
    /// per batch instead of one `sendto` each.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    fn send_batch(&mut self, datagrams: &[Vec<u8>]) -> Result<(), RpcError> {
        if !self.connected {
            for d in datagrams {
                self.send(d)?;
            }
            return Ok(());
        }
        use std::os::fd::AsRawFd;
        let fd = self.sock.as_raw_fd();
        let refs: Vec<&[u8]> = datagrams.iter().map(|d| d.as_slice()).collect();
        let mut off = 0;
        while off < refs.len() {
            match grpcx_io::udp_batch::send_batch(fd, &refs[off..]) {
                Ok(n) => off += n,
                Err(e) if e.kind() == grpcx_io::IoErrorKind::WouldBlock => {
                    std::thread::yield_now();
                }
                Err(e) if e.kind() == grpcx_io::IoErrorKind::ConnectionRefused => {
                    return Err(RpcError::Transport(crate::transport_common::UDP_REFUSED));
                }
                Err(_) => return Err(RpcError::Transport("udp: batch send failed")),
            }
        }
        Ok(())
    }

    /// Darwin `recvmsg_x` / Linux `recvmmsg`: one syscall
    /// pulls up to a batch of datagrams.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    fn recv_batch(&mut self, out: &mut Vec<Vec<u8>>) -> Result<usize, RpcError> {
        if !self.connected {
            // Unconnected = still peer-locking; take the
            // single-datagram path until the first sender
            // upgrades us.
            return match self.recv()? {
                Some(d) => {
                    out.push(d);
                    Ok(1)
                }
                None => Ok(0),
            };
        }
        use std::os::fd::AsRawFd;
        let fd = self.sock.as_raw_fd();
        let mut bufs: Vec<&mut [u8]> = self.batch_buf.chunks_mut(UDP_BATCH_SLOT).collect();
        let mut lens = [0usize; UDP_BATCH];
        match grpcx_io::udp_batch::recv_batch(fd, &mut bufs, &mut lens) {
            Ok(n) => {
                drop(bufs);
                for i in 0..n {
                    let slot = &self.batch_buf[i * UDP_BATCH_SLOT..];
                    out.push(slot[..lens[i]].to_vec());
                }
                Ok(n)
            }
            Err(e) if e.kind() == grpcx_io::IoErrorKind::WouldBlock => Ok(0),
            Err(e) if e.kind() == grpcx_io::IoErrorKind::ConnectionRefused => {
                Err(RpcError::Transport(crate::transport_common::UDP_REFUSED))
            }
            Err(_) => Err(RpcError::Transport("udp: batch recv failed")),
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
            self.parker = Some(crate::transport_common::FdParker::new(
                self.sock.as_raw_fd(),
            )?);
        }
        if let Some(p) = self.parker.as_mut() {
            p.park(timeout)?;
        }
        Ok(true)
    }
}

// ============================================================
// Server side.
// ============================================================

/// One accepted server-side H/3 connection. Owns the L5
/// session; [`pump`](Self::pump) moves datagrams,
/// [`dispatch`](Self::dispatch) routes completed requests
/// into the [`Server`]'s registered services.
pub struct ServerConn {
    conn: H3Connection,
    pending: Vec<grpcx_h3::RequestStream>,
    /// See `H3ClientInner::epoch` — same time stamping,
    /// server side.
    epoch: std::time::Instant,
}

impl Server {
    /// Create a server-side H/3 session (one per client
    /// connection). The caller shuttles datagrams between
    /// the returned [`ServerConn`] and its peer.
    pub fn accept_h3(&self, tls: &TlsConfig) -> Result<ServerConn, RpcError> {
        let quic = QuicConnection::new_server(tls, b"grpcx-s".to_vec())
            .map_err(|_| RpcError::Transport("quic server construction failed"))?;
        let mut conn = H3Connection::new_server(quic, Settings::default()).map_err(map_h3)?;
        // Server-side liveness is OFF by default (the gRPC
        // server convention): a blocking client doesn't
        // pump between calls, so probing/idle-killing
        // healthy-but-quiet clients would break connection
        // reuse. Opt in via `set_link_liveness` for
        // deployments that want idle-conn GC.
        conn.set_link_liveness_us(u64::MAX, u64::MAX);
        Ok(ServerConn {
            conn,
            pending: Vec::new(),
            epoch: std::time::Instant::now(),
        })
    }
}

impl ServerConn {
    /// Feed one incoming datagram (or `None`) and collect
    /// outgoing datagrams.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        let now_us = self.epoch.elapsed().as_micros() as u64;
        self.conn.pump_at(incoming, now_us).map_err(map_h3)
    }

    /// Set the QUIC 1-RTT plaintext budget per datagram —
    /// see [`Channel::set_datagram_budget`](crate::Channel::set_datagram_budget).
    pub fn set_datagram_budget(&mut self, plaintext_budget: usize) {
        self.conn.set_datagram_budget(plaintext_budget);
    }

    /// Tune this connection's link-liveness atom — see
    /// [`Channel::set_link_liveness`](crate::Channel::set_link_liveness).
    pub fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.conn.set_link_liveness_us(
            keepalive.as_micros() as u64,
            idle_timeout.as_micros() as u64,
        );
    }

    /// `true` once the session is established.
    pub fn is_ready(&self) -> bool {
        self.conn.is_ready()
    }

    /// Accept newly-opened request streams and dispatch
    /// every request whose headers + full body have
    /// arrived. Returns the number of RPCs dispatched this
    /// cycle.
    pub fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        while let Some(rs) = self.conn.accept_request().map_err(map_h3)? {
            self.pending.push(rs);
        }

        let mut dispatched = 0;
        let mut i = 0;
        while i < self.pending.len() {
            let headers_ready = self.conn.received_headers_ready(&self.pending[i]);
            let (_, fin) = self.conn.data_buffer_status(&self.pending[i]);
            if headers_ready && fin {
                let rs = self.pending.swap_remove(i);
                self.handle_one(server, &rs)?;
                dispatched += 1;
            } else {
                i += 1;
            }
        }
        Ok(dispatched)
    }

    fn handle_one(
        &mut self,
        server: &Server,
        rs: &grpcx_h3::RequestStream,
    ) -> Result<(), RpcError> {
        let hdrs = self
            .conn
            .read_headers(rs)
            .map_err(map_h3)?
            .expect("ready implies Some");
        let meta = match from_h3_headers(&hdrs) {
            Ok(m) => m,
            Err(_) => return self.respond_error(rs, StatusCode::InvalidArgument, "bad headers"),
        };
        let (service, method, user_meta) = match parse_request_headers(&meta) {
            Ok(t) => t,
            Err(_) => {
                return self.respond_error(rs, StatusCode::InvalidArgument, "malformed request");
            }
        };

        let mut body = Vec::new();
        let _ = self.conn.read_data(rs, &mut body).map_err(map_h3)?;
        let frames = match decode_all_frames(&body) {
            Ok(f) => f,
            Err(_) => return self.respond_error(rs, StatusCode::InvalidArgument, "bad frame"),
        };
        if frames.iter().any(|f| f.compressed) {
            return self.respond_error(rs, StatusCode::Unimplemented, "compression not negotiated");
        }
        let requests: Vec<Vec<u8>> = frames.into_iter().map(|f| f.payload).collect();

        let Some(svc) = server.find_service(&service) else {
            return self.respond_error(rs, StatusCode::Unimplemented, "unknown service");
        };

        // All four RpcKinds flow through the streaming
        // dispatch surface (unary services use its default
        // 1-in/1-out bridge). Each returned message gets
        // its own length-prefixed frame.
        match crate::dispatch_streaming_deadline(svc, &method, &user_meta, &requests) {
            Ok((responses, trailing)) => {
                self.conn
                    .write_headers(rs, &to_h3_headers(&encode_response_headers()))
                    .map_err(map_h3)?;
                for resp in responses {
                    write_message_frame(&mut self.conn, rs, &resp)?;
                }
                let trailers = encode_trailers(&Status::default(), &trailing);
                self.conn
                    .write_headers(rs, &to_h3_headers(&trailers))
                    .map_err(map_h3)?;
                self.conn.finish(rs).map_err(map_h3)?;
            }
            Err(RpcError::Status(status)) => {
                self.respond_status(rs, &status)?;
            }
            Err(_) => {
                self.respond_error(rs, StatusCode::Internal, "service failure")?;
            }
        }
        // Release the request's connection-side state —
        // without this every RPC leaks a dead stream entry
        // and the per-pump sweep degrades linearly (the
        // O(calls²) found by the v0.7 comparator).
        self.conn.close_request(rs);
        Ok(())
    }

    /// Trailers-only error response (the gRPC fast path):
    /// one HEADERS section carrying `:status 200` +
    /// `grpc-status` + `grpc-message`, then FIN.
    fn respond_status(
        &mut self,
        rs: &grpcx_h3::RequestStream,
        status: &Status,
    ) -> Result<(), RpcError> {
        let mut hdrs = encode_response_headers();
        hdrs.headers
            .extend(encode_trailers(status, &Metadata::default()).headers);
        self.conn
            .write_headers(rs, &to_h3_headers(&hdrs))
            .map_err(map_h3)?;
        self.conn.finish(rs).map_err(map_h3)
    }

    fn respond_error(
        &mut self,
        rs: &grpcx_h3::RequestStream,
        code: StatusCode,
        msg: &str,
    ) -> Result<(), RpcError> {
        self.respond_status(
            rs,
            &Status {
                code,
                message: msg.to_string(),
            },
        )
    }
}

// ============================================================
// Connected-channel H/3 arm (the core ConnectedChannel +
// the call facades live in `transport_common`; only the
// H/3 constructor attaches here).
// ============================================================

impl ConnectedChannel {
    /// Connect to a gRPC-over-H3 server at `addr`
    /// (`"host:port"` UDP) — binds an ephemeral local
    /// socket and constructs the H3 session. The QUIC/TLS
    /// handshake completes lazily on the first call.
    pub fn connect_h3(addr: &str, tls: &TlsConfig, server_name: &str) -> Result<Self, RpcError> {
        let mut transport = UdpTransport::bind("0.0.0.0:0", addr)?;
        // The peer is the real server address — OS-connect
        // now so the batch syscalls apply from the first
        // datagram.
        transport.upgrade_to_connected();
        let channel = crate::Channel::connect_h3(tls, server_name, addr)?;
        Ok(ConnectedChannel {
            channel,
            transport: Box::new(transport),
        })
    }
}

/// Per-connection driver task: mirror of `drive_tcp_conn`
/// with the router's inbox channel in place of a per-conn
/// fd. Owns the session + a cloned socket (concurrent
/// `send_to` on a shared UDP fd is kernel-safe); replies
/// go straight out — the router only ever reads.
/// Exits on conn error: one bad conn kills its task, not
/// the server (the TCP-serve lesson).
async fn drive_h3_conn(
    mut conn: ServerConn,
    rx: grpcx_rt::Receiver<alloc::vec::Vec<u8>>,
    sock: std::net::UdpSocket,
    peer: core::net::SocketAddr,
    server: crate::Server,
    stop: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
) {
    use core::sync::atomic::Ordering;
    let mut idle = 0u32;
    // Passive idle GC (0.25) — see drive_tcp_conn.
    let max_idle = server.max_connection_idle();
    let mut last_inbound = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if let Some(budget) = max_idle
            && last_inbound.elapsed() > budget
        {
            return; // reaped — router removes the inbox on Closed
        }
        let mut progressed = false;
        while let Ok(dg) = rx.try_recv() {
            progressed = true;
            last_inbound = std::time::Instant::now();
            let Ok(outs) = conn.pump(Some(&dg)) else {
                return;
            };
            for out in outs {
                // Best-effort, exactly like the previous
                // single-thread loop — a dropped reply is
                // QUIC loss recovery's job.
                let _ = sock.send_to(&out, peer);
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
            let _ = sock.send_to(&out, peer);
        }
        if progressed {
            idle = 0;
        } else if idle < 4 {
            idle += 1;
            grpcx_rt::yield_now().await;
        } else {
            // Park on the inbox; the 5 ms cap bounds both
            // stop-flag latency and conn timer cadence
            // (PTO / keepalive run off pump(None)).
            match grpcx_rt::timeout(core::time::Duration::from_millis(5), rx.recv()).await {
                Ok(Some(dg)) => {
                    last_inbound = std::time::Instant::now();
                    let Ok(outs) = conn.pump(Some(&dg)) else {
                        return;
                    };
                    for out in outs {
                        let _ = sock.send_to(&out, peer);
                    }
                    idle = 0;
                }
                // Router dropped the sender — shutdown.
                Ok(None) => return,
                Err(_) => {}
            }
        }
    }
}

impl Server {
    /// Bind `addr` (UDP) and serve registered services
    /// until `stop` flips true — multi-core: the caller
    /// thread is the ROUTER (owns the socket, routes
    /// datagrams to per-peer sessions by source address,
    /// parks on the socket fd when quiet); each connection
    /// runs as its own task on a `grpcx_rt::Runtime`
    /// (one slow handler no longer stalls — and one broken
    /// conn no longer kills — every other connection).
    pub fn listen_h3_until(
        &self,
        addr: &str,
        tls: &TlsConfig,
        stop: &core::sync::atomic::AtomicBool,
    ) -> Result<(), RpcError> {
        use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::collections::HashMap;

        let sock =
            std::net::UdpSocket::bind(addr).map_err(|_| RpcError::Transport("udp: bind failed"))?;
        sock.set_nonblocking(true)
            .map_err(|_| RpcError::Transport("udp: nonblocking failed"))?;
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 8);
        let rt = grpcx_rt::Runtime::new(workers)
            .map_err(|_| RpcError::Transport("rt: runtime init failed"))?;
        let task_stop = alloc::sync::Arc::new(AtomicBool::new(false));
        let live = alloc::sync::Arc::new(AtomicUsize::new(0));

        let mut buf = vec![0u8; 65535];
        let mut inboxes: HashMap<core::net::SocketAddr, grpcx_rt::Sender<alloc::vec::Vec<u8>>> =
            HashMap::new();
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd"
        ))]
        let mut parker: Option<crate::transport_common::FdParker> = None;

        while !stop.load(Ordering::Relaxed) {
            let mut progressed = false;
            // Drain the socket, routing by peer address.
            loop {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        progressed = true;
                        let dg = buf[..n].to_vec();
                        let dg = match inboxes.get(&from) {
                            Some(tx) => match tx.try_send(dg) {
                                Ok(()) => continue,
                                // Inbox full: DROP — UDP
                                // semantics, QUIC loss
                                // recovery owns it
                                // (backpressure by loss).
                                Err(grpcx_rt::TrySendError::Full(_)) => continue,
                                // Conn task exited — fall
                                // through to a fresh accept
                                // (matches the old loop's
                                // unknown-peer behavior).
                                Err(grpcx_rt::TrySendError::Closed(dg)) => {
                                    inboxes.remove(&from);
                                    dg
                                }
                            },
                            None => dg,
                        };
                        let conn = self.accept_h3(tls)?;
                        let (tx, rx) = grpcx_rt::channel(256);
                        let _ = tx.try_send(dg);
                        inboxes.insert(from, tx);
                        let sock_clone = sock
                            .try_clone()
                            .map_err(|_| RpcError::Transport("udp: clone failed"))?;
                        let server = self.clone();
                        let task_stop = alloc::sync::Arc::clone(&task_stop);
                        let live = alloc::sync::Arc::clone(&live);
                        live.fetch_add(1, Ordering::AcqRel);
                        rt.spawn(async move {
                            drive_h3_conn(conn, rx, sock_clone, from, server, task_stop).await;
                            live.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        task_stop.store(true, Ordering::Relaxed);
                        return Err(RpcError::Transport("udp: recv failed"));
                    }
                }
            }
            if !progressed {
                // Quiet socket: park in the reactor for one
                // stall slice (bounds stop-flag latency to
                // ~500 µs) instead of yield-spinning.
                #[cfg(any(
                    target_os = "linux",
                    target_os = "macos",
                    target_os = "ios",
                    target_os = "freebsd"
                ))]
                {
                    use std::os::fd::AsRawFd;
                    if parker.is_none() {
                        parker = Some(crate::transport_common::FdParker::new(sock.as_raw_fd())?);
                    }
                    if let Some(p) = parker.as_mut() {
                        p.park(crate::transport_common::PARK_SLICE)?;
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
        }

        // Wind down: drop the inboxes so parked workers see
        // the disconnect, mirror the stop flag, dead-man cap
        // exactly like the TCP serve loop.
        task_stop.store(true, Ordering::Relaxed);
        inboxes.clear();
        let deadline = std::time::Instant::now() + core::time::Duration::from_secs(2);
        while live.load(Ordering::Acquire) > 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        Ok(())
    }

    /// [`listen_h3_until`](Self::listen_h3_until) without
    /// a stop flag — serves forever (the tonic
    /// `serve(addr)` shape).
    pub fn listen_h3(&self, addr: &str, tls: &TlsConfig) -> Result<(), RpcError> {
        let stop = core::sync::atomic::AtomicBool::new(false);
        self.listen_h3_until(addr, tls, &stop)
    }
}
