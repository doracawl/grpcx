// `grpcx/ws-mux` transport — v0.6 (work unit 6).
//
// WebSocket (L9) carries one binary message per mux frame;
// the mux frame routes it to a logical stream (SPEC.md
// § grpcx/ws-mux). Pure `no_std + alloc`: the WS bytes go
// through the same sans-IO pump shape as the H/3 path —
// feed inbound TCP bytes, collect outbound byte chunks.
//
// The mux layer itself (frame grammar, routing, unary /
// dispatch logic) lives in `mux.rs`, shared with
// `grpcx/tcp-mux` (T7) — this module is the WebSocket
// shell: RFC 6455 upgrade handshake + binary-message
// framing + client-side masking.

use crate::mux::{MUX_DATA, MUX_HEADERS, RecvStream, route_mux_frame};
use crate::{
    MessageFrame, Metadata, Method, RpcError, Server, encode_frame, encode_request_headers,
    mux::{dispatch_one_mux, encode_header_list, encode_mux_frame, poll_unary_mux},
};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use grpcx_ws::{Frame, Handshake, Opcode, Reassembler, ReassemblyOutput};

// ============================================================
// Shared WS plumbing.
// ============================================================

/// Drain complete WS frames out of `in_buf`, reassemble
/// messages, and route binary payloads into `streams`.
/// Control frames (ping/close) are ignored at v0.6 — the
/// loopback harness doesn't emit them; v1.x answers pings.
/// Drain complete WS messages into per-stream mux state.
/// Link-level PINGs answer with mux-encoded PONGs pushed
/// to `pongs` — the caller wraps them in WS frames (the
/// client masks, the server doesn't).
fn drain_ws_frames(
    in_buf: &mut Vec<u8>,
    reasm: &mut Reassembler,
    streams: &mut BTreeMap<u32, RecvStream>,
    pongs: &mut Vec<Vec<u8>>,
) -> Result<(), RpcError> {
    loop {
        match grpcx_ws::decode_frame(in_buf) {
            Ok((frame, consumed)) => {
                in_buf.drain(..consumed);
                match reasm.feed(frame) {
                    Ok(ReassemblyOutput::Message(msg)) => {
                        if let Some(pong) = route_mux_frame(streams, &msg.payload)? {
                            pongs.push(pong);
                        }
                    }
                    Ok(_) => {} // Pending fragment / control
                    Err(_) => return Err(RpcError::Transport("ws: protocol error")),
                }
            }
            Err(grpcx_ws::WsError::Truncated) => return Ok(()),
            Err(_) => return Err(RpcError::Transport("ws: bad frame")),
        }
    }
}

fn binary_frame(payload: Vec<u8>, mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    grpcx_ws::encode_frame(
        &Frame {
            fin: true,
            rsv1: false,
            rsv2: false,
            rsv3: false,
            opcode: Opcode::Binary,
            mask,
            payload,
        },
        &mut out,
    );
    out
}

// ============================================================
// Client.
// ============================================================

enum WsClientState {
    /// Upgrade request not yet emitted.
    Start,
    /// Upgrade sent; awaiting the 101 response.
    AwaitUpgrade,
    /// Mux frames flowing.
    Open,
}

/// Client side of a `grpcx/ws-mux` connection (sans-IO).
pub(crate) struct WsMuxClient {
    state: WsClientState,
    host: String,
    authority: String,
    request_key: String,
    in_buf: Vec<u8>,
    out_queue: Vec<Vec<u8>>,
    reasm: Reassembler,
    streams: BTreeMap<u32, RecvStream>,
    next_stream_id: u32,
    mask_state: u64,
    liveness: crate::mux::LinkLiveness,
}

impl WsMuxClient {
    pub(crate) fn new(host: &str, authority: &str, handshake_key: &str, mask_seed: u64) -> Self {
        WsMuxClient {
            state: WsClientState::Start,
            host: host.to_string(),
            authority: authority.to_string(),
            request_key: handshake_key.to_string(),
            in_buf: Vec::new(),
            out_queue: Vec::new(),
            reasm: Reassembler::new(),
            streams: BTreeMap::new(),
            next_stream_id: 1,
            mask_state: mask_seed,
            liveness: crate::mux::LinkLiveness::new(),
        }
    }

    pub(crate) fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.liveness.configure(keepalive, idle_timeout);
    }

    /// Next client→server mask key. NOT cryptographically
    /// random (SPEC.md masking note): RFC 6455 masking
    /// defends proxy cache poisoning, not confidentiality;
    /// TLS is the security boundary.
    fn next_mask(&mut self) -> [u8; 4] {
        self.mask_state = self
            .mask_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.mask_state >> 32) as u32).to_be_bytes()
    }

    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.state, WsClientState::Open)
    }

    pub(crate) fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        if let Some(bytes) = incoming
            && !bytes.is_empty()
        {
            self.liveness.on_inbound();
            self.in_buf.extend_from_slice(bytes);
        }

        if matches!(self.state, WsClientState::Start) {
            let hs = Handshake {
                request_key: self.request_key.clone(),
                ..Handshake::default()
            };
            let req = grpcx_ws::perform_client_handshake_request(&self.host, "/grpcx-ws-mux", &hs);
            self.out_queue.push(req.into_bytes());
            self.state = WsClientState::AwaitUpgrade;
        }

        if matches!(self.state, WsClientState::AwaitUpgrade)
            && let Some(end) = find_header_end(&self.in_buf)
        {
            let head: Vec<u8> = self.in_buf.drain(..end).collect();
            let hs = grpcx_ws::parse_server_handshake_response(&head)
                .map_err(|_| RpcError::Transport("ws: bad upgrade response"))?;
            grpcx_ws::verify_accept_key(&self.request_key, &hs.accept_key)
                .map_err(|_| RpcError::Transport("ws: accept-key mismatch"))?;
            self.state = WsClientState::Open;
        }

        if matches!(self.state, WsClientState::Open) {
            let mut pongs = Vec::new();
            drain_ws_frames(
                &mut self.in_buf,
                &mut self.reasm,
                &mut self.streams,
                &mut pongs,
            )?;
            for pong in pongs {
                let mask = self.next_mask();
                self.out_queue.push(binary_frame(pong, Some(mask)));
            }
        }
        if self.liveness.check(self.is_ready(), "ws-mux: link dead")? {
            let ping = encode_mux_frame(0, crate::mux::MUX_PING, false, &[]);
            let mask = self.next_mask();
            self.out_queue.push(binary_frame(ping, Some(mask)));
        }

        Ok(core::mem::take(&mut self.out_queue))
    }

    /// Open a unary stream: HEADERS, then one DATA frame
    /// carrying the gRPC length-prefixed request, with
    /// END_STREAM.
    pub(crate) fn open_unary(&mut self, headers: &Metadata, grpc_frame: &[u8]) -> u32 {
        let sid = self.next_stream_id;
        self.next_stream_id += 2; // odd ids, client side

        let h = encode_mux_frame(sid, MUX_HEADERS, false, &encode_header_list(headers));
        let mask = self.next_mask();
        self.out_queue.push(binary_frame(h, Some(mask)));

        let d = encode_mux_frame(sid, MUX_DATA, true, grpc_frame);
        let mask = self.next_mask();
        self.out_queue.push(binary_frame(d, Some(mask)));

        self.streams.insert(sid, RecvStream::default());
        sid
    }

    /// Eager cancel (0.22): MUX_RST for the sid (masked,
    /// client side) + local state dropped; flushed by the
    /// next pump.
    fn cancel(&mut self, sid: u32) {
        let f = encode_mux_frame(sid, crate::mux::MUX_RST, false, &[]);
        let mask = self.next_mask();
        self.out_queue.push(binary_frame(f, Some(mask)));
        self.streams.remove(&sid);
    }

    /// Open a streaming-shaped stream: HEADERS, then one
    /// DATA frame per gRPC message, END_STREAM on the last
    /// (or an empty END_STREAM DATA when there are no
    /// requests) — the tcp-mux batch shape over masked WS
    /// binary frames.
    fn open_streaming(&mut self, headers: &Metadata, grpc_frames: &[Vec<u8>]) -> u32 {
        let sid = self.next_stream_id;
        self.next_stream_id += 2; // odd ids, client side
        let h = encode_mux_frame(sid, MUX_HEADERS, false, &encode_header_list(headers));
        let mask = self.next_mask();
        self.out_queue.push(binary_frame(h, Some(mask)));
        if grpc_frames.is_empty() {
            let d = encode_mux_frame(sid, MUX_DATA, true, &[]);
            let mask = self.next_mask();
            self.out_queue.push(binary_frame(d, Some(mask)));
        } else {
            for (i, frame) in grpc_frames.iter().enumerate() {
                let last = i + 1 == grpc_frames.len();
                let d = encode_mux_frame(sid, MUX_DATA, last, frame);
                let mask = self.next_mask();
                self.out_queue.push(binary_frame(d, Some(mask)));
            }
        }
        self.streams.insert(sid, RecvStream::default());
        sid
    }

    /// Check a unary stream for completion. Mirrors the
    /// H/3 unary semantics including the trailers-only
    /// error fast path.
    pub(crate) fn poll_unary(&mut self, sid: u32) -> Result<Option<(Vec<u8>, Metadata)>, RpcError> {
        poll_unary_mux(&mut self.streams, sid)
    }
}

/// The streaming core every ws-mux facade rides (the
/// tcp-mux shape): all request messages out (batch),
/// every response message collected until the stream
/// completes. Establishment (the WS upgrade) counts
/// against the caller's deadline, per gRPC semantics.
pub(crate) fn call_streaming_ws_mux(
    channel: &mut crate::Channel,
    transport: &mut dyn crate::transport_common::DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    use crate::transport_common::drive_once;
    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    while !channel.is_ready() {
        deadline.check()?;
        idle = drive_once(channel, transport, idle)?;
    }
    let inner = channel
        .wsmux
        .as_mut()
        .ok_or(RpcError::TransportUnavailable)?;
    let authority = inner.authority().to_string();
    let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
    let grpc_frames: Vec<Vec<u8>> = requests
        .iter()
        .map(|r| {
            let mut wire = Vec::new();
            encode_frame(
                &MessageFrame {
                    compressed: false,
                    payload: r.to_vec(),
                },
                &mut wire,
            );
            wire
        })
        .collect();
    let sid = inner.open_streaming(&hdrs, &grpc_frames);

    let mut idle = 0u32;
    loop {
        if let Err(e) = deadline.check() {
            // Eager cancel (0.22): MUX_RST + best-effort
            // flush, then the verdict.
            let inner = channel.wsmux.as_mut().expect("checked above");
            inner.cancel(sid);
            if let Ok(outs) = channel.pump(None) {
                for out in outs {
                    let _ = transport.send(&out);
                }
            }
            return Err(e);
        }
        let inner = channel.wsmux.as_mut().expect("checked above");
        if let Some(done) = crate::mux::poll_completed_mux(&mut inner.streams, sid)? {
            return Ok(done);
        }
        idle = drive_once(channel, transport, idle)?;
    }
}

/// Blocking unary over ws-mux — dispatched from the
/// shared `call_unary`.
pub(crate) fn call_unary_ws_mux(
    channel: &mut crate::Channel,
    transport: &mut dyn crate::transport_common::DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    let (msgs, trailing) = call_streaming_ws_mux(channel, transport, method, metadata, &[request])?;
    Ok((crate::transport_common::first_or_empty(msgs), trailing))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

// ============================================================
// Server.
// ============================================================

enum WsServerState {
    AwaitUpgrade,
    Open,
}

/// Server side of one `grpcx/ws-mux` connection
/// (sans-IO). Constructed by [`Server::accept_ws_mux`],
/// driven with [`pump`](Self::pump) +
/// [`dispatch`](Self::dispatch).
pub struct WsMuxServerConn {
    state: WsServerState,
    in_buf: Vec<u8>,
    out_queue: Vec<Vec<u8>>,
    reasm: Reassembler,
    streams: BTreeMap<u32, RecvStream>,
    liveness: crate::mux::LinkLiveness,
}

impl Server {
    /// Create the server side of a ws-mux connection. The
    /// caller shuttles bytes between the returned
    /// [`WsMuxServerConn`] and its peer (TCP once the IO
    /// runtime lands; in-memory in tests).
    pub fn accept_ws_mux(&self) -> WsMuxServerConn {
        let mut liveness = crate::mux::LinkLiveness::new();
        // Server-side liveness OFF by default — see
        // `Server::accept_h3`.
        liveness.configure(core::time::Duration::MAX, core::time::Duration::MAX);
        WsMuxServerConn {
            state: WsServerState::AwaitUpgrade,
            in_buf: Vec::new(),
            out_queue: Vec::new(),
            reasm: Reassembler::new(),
            streams: BTreeMap::new(),
            liveness,
        }
    }
}

impl WsMuxServerConn {
    /// `true` once the WS upgrade completed.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, WsServerState::Open)
    }

    /// Tune this connection's link-liveness atom — see
    /// [`Channel::set_link_liveness`](crate::Channel::set_link_liveness).
    pub fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.liveness.configure(keepalive, idle_timeout);
    }

    /// Feed inbound bytes (or `None`) and collect outbound
    /// byte chunks.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        if let Some(bytes) = incoming
            && !bytes.is_empty()
        {
            self.liveness.on_inbound();
            self.in_buf.extend_from_slice(bytes);
        }

        if matches!(self.state, WsServerState::AwaitUpgrade)
            && let Some(end) = find_header_end(&self.in_buf)
        {
            let head: Vec<u8> = self.in_buf.drain(..end).collect();
            let hs = grpcx_ws::parse_client_handshake_request(&head)
                .map_err(|_| RpcError::Transport("ws: bad upgrade request"))?;
            let mut resp_hs = hs.clone();
            resp_hs.accept_key = grpcx_ws::derive_accept_key(&hs.request_key);
            let resp = grpcx_ws::build_server_handshake_response(&resp_hs);
            self.out_queue.push(resp.into_bytes());
            self.state = WsServerState::Open;
        }

        if matches!(self.state, WsServerState::Open) {
            let mut pongs = Vec::new();
            drain_ws_frames(
                &mut self.in_buf,
                &mut self.reasm,
                &mut self.streams,
                &mut pongs,
            )?;
            for pong in pongs {
                // Server→client frames go unmasked.
                self.out_queue.push(binary_frame(pong, None));
            }
        }
        if self.liveness.check(self.is_ready(), "ws-mux: link dead")? {
            let ping = encode_mux_frame(0, crate::mux::MUX_PING, false, &[]);
            self.out_queue.push(binary_frame(ping, None));
        }

        Ok(core::mem::take(&mut self.out_queue))
    }

    /// Dispatch every stream whose request is complete
    /// (headers + END_STREAM). Returns the number of RPCs
    /// dispatched.
    pub fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        let ready: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, st)| st.leading.is_some() && st.end_seen)
            .map(|(sid, _)| *sid)
            .collect();

        let mut dispatched = 0;
        for sid in ready {
            let st = self.streams.remove(&sid).expect("collected above");
            for mux in dispatch_one_mux(server, sid, st)? {
                // Server→client frames go unmasked (RFC
                // 6455 §5.1).
                self.out_queue.push(binary_frame(mux, None));
            }
            dispatched += 1;
        }
        Ok(dispatched)
    }
}

// ============================================================
// Channel integration (unary over ws-mux).
// ============================================================

/// In-flight unary call over `grpcx/ws-mux` — created by
/// [`Channel::begin_unary_ws`](crate::Channel::begin_unary_ws),
/// advanced by
/// [`Channel::poll_unary_ws`](crate::Channel::poll_unary_ws).
/// (Transport-unified call objects are the v0.7 API pass.)
pub struct WsUnaryCall {
    stream_id: u32,
}

impl crate::Channel {
    /// Open a sans-IO `grpcx/ws-mux` channel. The caller
    /// supplies the RFC 6455 `Sec-WebSocket-Key` nonce and
    /// a mask seed (this crate carries no RNG — see
    /// SPEC.md masking note).
    pub fn connect_ws_mux(
        host: &str,
        authority: &str,
        handshake_key: &str,
        mask_seed: u64,
    ) -> Self {
        crate::Channel {
            transport: crate::TransportKind::WsMux,
            #[cfg(feature = "transport-h3")]
            h3: None,
            wsmux: Some(WsMuxClient::new(host, authority, handshake_key, mask_seed)),
            #[cfg(feature = "transport-h2")]
            h2: None,
            #[cfg(feature = "transport-tcp-mux")]
            tcpmux: None,
        }
    }

    fn ws_inner(&mut self) -> Result<&mut WsMuxClient, RpcError> {
        self.wsmux.as_mut().ok_or(RpcError::TransportUnavailable)
    }

    /// Feed inbound bytes / collect outbound bytes for a
    /// ws-mux channel.
    pub fn pump_ws(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        self.ws_inner()?.pump(incoming)
    }

    /// `true` once the WS upgrade handshake completed.
    pub fn is_ready_ws(&self) -> bool {
        self.wsmux.as_ref().is_some_and(WsMuxClient::is_ready)
    }

    /// Start a unary call over ws-mux.
    pub fn begin_unary_ws(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<WsUnaryCall, RpcError> {
        let ws = self.ws_inner()?;
        let authority = ws.authority().to_string();
        let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
        let mut grpc_frame = Vec::new();
        encode_frame(
            &MessageFrame {
                compressed: false,
                payload: request.to_vec(),
            },
            &mut grpc_frame,
        );
        let stream_id = ws.open_unary(&hdrs, &grpc_frame);
        Ok(WsUnaryCall { stream_id })
    }

    /// Advance a ws-mux unary call (same semantics as the
    /// H/3 [`poll_unary`](Self::poll_unary)).
    pub fn poll_unary_ws(
        &mut self,
        call: &WsUnaryCall,
    ) -> Result<Option<(Vec<u8>, Metadata)>, RpcError> {
        self.ws_inner()?.poll_unary(call.stream_id)
    }
}

// ============================================================
// Socket facades (0.19 — ws-mux as a full transport).
// ============================================================

/// Non-cryptographic 16-byte WS nonce + mask seed from
/// wall clock + address hash. Per SPEC's masking note:
/// RFC 6455 masking defends proxy cache poisoning, not
/// confidentiality — TLS is the security boundary; the
/// nonce only needs per-connection uniqueness.
fn derive_ws_entropy(addr: &str) -> (String, u64) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in addr.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed = h ^ now;
    let mut nonce = [0u8; 16];
    let mut x = seed;
    for chunk in nonce.chunks_mut(8) {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        chunk.copy_from_slice(&x.to_be_bytes());
    }
    (base64_16(&nonce), seed)
}

/// Standard-alphabet base64 of exactly 16 bytes (the WS
/// nonce shape: 22 chars + `==`).
fn base64_16(bytes: &[u8; 16]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(24);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHA[(n >> 18) as usize & 63]);
        out.push(ALPHA[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            ALPHA[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[n as usize & 63]
        } else {
            b'='
        });
    }
    String::from_utf8(out).expect("base64 is ASCII")
}

impl crate::transport_common::ConnectedChannel {
    /// Connect a `grpcx/ws-mux` channel over a real TCP
    /// socket: the WS Upgrade handshake completes through
    /// the blocking drive; `authority` lands in every
    /// call's `:authority`-equivalent header.
    pub fn connect_ws_mux(addr: &str, authority: &str) -> Result<Self, RpcError> {
        let transport = crate::transport_common::TcpTransport::connect(addr)?;
        let (key, seed) = derive_ws_entropy(addr);
        let channel = crate::Channel::connect_ws_mux(addr, authority, &key, seed);
        Ok(crate::transport_common::ConnectedChannel::new(
            channel,
            alloc::boxed::Box::new(transport),
        ))
    }
}

#[cfg(feature = "transport-h2-tls")]
impl crate::transport_common::ConnectedChannel {
    /// [`connect_ws_mux`](Self::connect_ws_mux) over
    /// TLS 1.3: the WS upgrade + mux grammar ride
    /// [`TlsTransport`](crate::TlsTransport) exactly as
    /// the h2 form does. Available when both
    /// `transport-ws-mux` and `transport-h2-tls` are on
    /// (both default; no separate feature — recorded in
    /// REFACTOR-v10-wsmux-tls.md).
    pub fn connect_ws_mux_tls(
        addr: &str,
        tls: &grpcx_crypto::TlsConfig,
        server_name: &str,
        authority: &str,
    ) -> Result<Self, RpcError> {
        let transport = crate::TlsTransport::connect(addr, tls, server_name)?;
        let (key, seed) = derive_ws_entropy(addr);
        let channel = crate::Channel::connect_ws_mux(addr, authority, &key, seed);
        Ok(crate::transport_common::ConnectedChannel::new(
            channel,
            alloc::boxed::Box::new(transport),
        ))
    }
}

impl crate::transport_common::TcpServeConn for WsMuxServerConn {
    fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        WsMuxServerConn::pump(self, incoming)
    }
    fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        WsMuxServerConn::dispatch(self, server)
    }
}

impl Server {
    /// Bind a TCP listener and serve `grpcx/ws-mux` until
    /// `stop` flips — the threaded L2-Runtime accept loop
    /// (one task per connection), same as the other TCP
    /// transports.
    pub fn listen_ws_mux_until(
        &self,
        addr: &str,
        stop: &core::sync::atomic::AtomicBool,
    ) -> Result<(), RpcError> {
        crate::transport_common::serve_tcp_until(self, addr, stop, |s| s.accept_ws_mux())
    }

    /// [`listen_ws_mux_until`](Self::listen_ws_mux_until)
    /// without a stop flag — serves forever.
    pub fn listen_ws_mux(&self, addr: &str) -> Result<(), RpcError> {
        let stop = core::sync::atomic::AtomicBool::new(false);
        self.listen_ws_mux_until(addr, &stop)
    }

    /// `grpcx/ws-mux` over TLS 1.3 — per-connection TLS
    /// handshakes advance non-blockingly inside the shared
    /// accept loop, exactly like
    /// [`listen_h2_until`](Self::listen_h2_until).
    #[cfg(feature = "transport-h2-tls")]
    pub fn listen_ws_mux_tls_until(
        &self,
        addr: &str,
        tls: &grpcx_crypto::TlsConfig,
        stop: &core::sync::atomic::AtomicBool,
    ) -> Result<(), RpcError> {
        crate::transport_common::serve_tcp_until_with(
            self,
            addr,
            stop,
            |s| s.accept_ws_mux(),
            |stream| crate::TlsTransport::accept_stream(stream, tls),
        )
    }

    /// [`listen_ws_mux_tls_until`](Self::listen_ws_mux_tls_until)
    /// without a stop flag — serves forever.
    #[cfg(feature = "transport-h2-tls")]
    pub fn listen_ws_mux_tls(
        &self,
        addr: &str,
        tls: &grpcx_crypto::TlsConfig,
    ) -> Result<(), RpcError> {
        let stop = core::sync::atomic::AtomicBool::new(false);
        self.listen_ws_mux_tls_until(addr, tls, &stop)
    }
}
