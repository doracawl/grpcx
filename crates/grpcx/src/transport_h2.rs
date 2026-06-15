// `grpcx/h2c` transport — the interop bridge (charter
// 2026-06-11). Cleartext HTTP/2 prior-knowledge over TCP:
// exactly what tonic's `http://` endpoints and grpc-go's
// insecure listeners speak. The TLS form (h2) rides this
// same module once L3's stream wrapper lands.
//
// Generated stubs are untouched: `ConnectedChannel::
// call_unary` → free `call_unary` → dispatches here when
// the channel is h2-flavoured.

use crate::transport_common::{ConnectedChannel, DatagramTransport, TcpTransport, drive_once};
use crate::{
    MessageFrame, Metadata, Method, RpcError, Server, Status, StatusCode, decode_all_frames,
    encode_frame, encode_request_headers, encode_response_headers, encode_trailers,
    parse_request_headers, parse_trailers,
};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use grpcx_h2::{Connection as H2Connection, Header, RequestStream};

fn map_h2(e: grpcx_h2::H2Error) -> RpcError {
    RpcError::Transport(match e {
        grpcx_h2::H2Error::LinkDead => "h2: link dead",
        _ => "h2: protocol error",
    })
}

fn to_pairs(meta: &Metadata) -> Vec<(String, Vec<u8>)> {
    meta.headers.clone()
}

fn from_h2_headers(hdrs: &[Header]) -> Result<Metadata, RpcError> {
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

pub(crate) struct H2ClientInner {
    conn: H2Connection,
    authority: String,
    /// Per-connection monotonic epoch — pumps are stamped
    /// `epoch.elapsed()` µs so the h2 liveness timers run
    /// (the L4 pattern).
    epoch: std::time::Instant,
}

impl H2ClientInner {
    pub(crate) fn is_ready(&self) -> bool {
        self.conn.is_ready()
    }

    pub(crate) fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.conn.set_keepalive_us(keepalive.as_micros() as u64);
        self.conn
            .set_idle_timeout_us(idle_timeout.as_micros() as u64);
    }
}

pub(crate) fn pump_h2(
    channel: &mut crate::Channel,
    incoming: Option<&[u8]>,
) -> Result<Vec<Vec<u8>>, RpcError> {
    let inner = channel.h2.as_mut().ok_or(RpcError::TransportUnavailable)?;
    let now_us = inner.epoch.elapsed().as_micros() as u64;
    inner.conn.pump_at(incoming, now_us).map_err(map_h2)
}

impl crate::Channel {
    /// Open a sans-IO h2c session (cleartext HTTP/2 prior
    /// knowledge). The preface/SETTINGS handshake
    /// completes through [`pump`](Self::pump) cycles.
    pub fn connect_h2c_session(authority: &str) -> Self {
        crate::Channel {
            transport: crate::TransportKind::H2c,
            #[cfg(feature = "transport-h3")]
            h3: None,
            #[cfg(feature = "transport-ws-mux")]
            wsmux: None,
            h2: Some(H2ClientInner {
                conn: H2Connection::new_client(),
                authority: authority.to_string(),
                epoch: std::time::Instant::now(),
            }),
            #[cfg(feature = "transport-tcp-mux")]
            tcpmux: None,
        }
    }
}

/// The streaming core every h2c facade rides: all request
/// messages out (batch shape: HEADERS + one gRPC frame
/// per message + half-close), every response message
/// collected until the trailers arrive.
pub(crate) fn call_streaming_h2(
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

    let authority = channel
        .h2
        .as_ref()
        .ok_or(RpcError::TransportUnavailable)?
        .authority
        .clone();
    let hdrs = encode_request_headers(&method.service, &method.method, &authority, metadata);
    let inner = channel.h2.as_mut().expect("checked above");
    let stream = inner.conn.send_request(&to_pairs(&hdrs));
    for r in requests {
        let mut wire = Vec::new();
        encode_frame(
            &MessageFrame {
                compressed: false,
                payload: r.to_vec(),
            },
            &mut wire,
        );
        inner.conn.write_data(&stream, &wire);
    }
    inner.conn.finish(&stream);

    // poll loop.
    let mut leading: Option<Metadata> = None;
    let mut body = Vec::new();
    let mut idle = 0u32;
    loop {
        let inner = channel.h2.as_mut().expect("h2 channel");
        if leading.is_none()
            && let Some(h) = inner.conn.read_headers(&stream)
        {
            let meta = from_h2_headers(&h)?;
            // Trailers-only response.
            if meta.headers.iter().any(|(n, _)| n == "grpc-status") {
                let (status, trailing) = parse_trailers(&meta)?;
                inner.conn.close_stream(&stream);
                return if status.code == StatusCode::Ok {
                    Ok((Vec::new(), trailing))
                } else {
                    Err(RpcError::Status(status))
                };
            }
            if !meta
                .headers
                .iter()
                .any(|(n, v)| n == ":status" && v == b"200")
            {
                inner.conn.close_stream(&stream);
                return Err(RpcError::Transport("h2: non-200 response"));
            }
            leading = Some(meta);
        }
        inner.conn.read_data(&stream, &mut body);
        if inner.conn.received_trailers_ready(&stream) {
            let trailers = inner.conn.read_trailers(&stream).expect("ready");
            let (status, trailing) = parse_trailers(&from_h2_headers(&trailers)?)?;
            inner.conn.close_stream(&stream);
            if status.code != StatusCode::Ok {
                return Err(RpcError::Status(status));
            }
            let frames = decode_all_frames(&body)?;
            if frames.iter().any(|f| f.compressed) {
                return Err(RpcError::Codec("unexpected compressed response"));
            }
            return Ok((frames.into_iter().map(|f| f.payload).collect(), trailing));
        }
        if let Err(e) = deadline.check() {
            // Eager cancel (0.22): RST_STREAM(CANCEL) on
            // the wire, best-effort flush, then the
            // verdict.
            if let Some(inner) = channel.h2.as_mut() {
                inner.conn.send_rst_stream(&stream, 0x8);
            }
            if let Ok(outs) = channel.pump(None) {
                for out in outs {
                    let _ = transport.send(&out);
                }
            }
            return Err(e);
        }
        idle = drive_once(channel, transport, idle)?;
    }
}

/// Blocking unary over h2c — dispatched from the shared
/// `call_unary`.
pub(crate) fn call_unary_h2(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    let (msgs, trailing) = call_streaming_h2(channel, transport, method, metadata, &[request])?;
    Ok((crate::transport_common::first_or_empty(msgs), trailing))
}

impl ConnectedChannel {
    /// Connect to a cleartext gRPC-over-h2c server at
    /// `addr` (`"host:port"` TCP) — what tonic's
    /// `http://host:port` endpoints speak.
    pub fn connect_h2c(addr: &str, authority: &str) -> Result<Self, RpcError> {
        let transport = TcpTransport::connect(addr)?;
        let channel = crate::Channel::connect_h2c_session(authority);
        Ok(ConnectedChannel::new(channel, Box::new(transport)))
    }
}

// ============================================================
// Server side.
// ============================================================

/// One accepted h2c connection (sans-IO; same shape as
/// the H/3 `ServerConn`).
pub struct H2ServerConn {
    conn: H2Connection,
    pending: Vec<RequestStream>,
    /// See `H2ClientInner::epoch`.
    epoch: std::time::Instant,
    /// Request bodies drained out of the connection while
    /// waiting for END_STREAM (`read_data` drains, so the
    /// partial body has to live somewhere).
    bodies: BTreeMap<u32, Vec<u8>>,
}

impl Server {
    /// Create the server side of an h2c session.
    pub fn accept_h2c(&self) -> H2ServerConn {
        let mut conn = H2Connection::new_server();
        // Server-side liveness OFF by default — see
        // `Server::accept_h3` (the gRPC server convention;
        // opt in via `set_link_liveness`).
        conn.set_keepalive_us(u64::MAX);
        conn.set_idle_timeout_us(u64::MAX);
        H2ServerConn {
            conn,
            pending: Vec::new(),
            bodies: BTreeMap::new(),
            epoch: std::time::Instant::now(),
        }
    }

    /// Bind a TCP listener and serve h2c until `stop`
    /// flips. Single-threaded multi-connection (same
    /// bootstrap-parity shape as `listen_h3_until`).
    pub fn listen_h2c_until(
        &self,
        addr: &str,
        stop: &core::sync::atomic::AtomicBool,
    ) -> Result<(), RpcError> {
        crate::transport_common::serve_tcp_until(self, addr, stop, |s| s.accept_h2c())
    }

    /// [`listen_h2c_until`](Self::listen_h2c_until)
    /// without a stop flag — serves forever.
    pub fn listen_h2c(&self, addr: &str) -> Result<(), RpcError> {
        let stop = core::sync::atomic::AtomicBool::new(false);
        self.listen_h2c_until(addr, &stop)
    }
}

impl crate::transport_common::TcpServeConn for H2ServerConn {
    fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        H2ServerConn::pump(self, incoming)
    }
    fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        H2ServerConn::dispatch(self, server)
    }
}

impl H2ServerConn {
    /// Tune this connection's link-liveness atom — see
    /// [`Channel::set_link_liveness`](crate::Channel::set_link_liveness).
    pub fn set_link_liveness(
        &mut self,
        keepalive: core::time::Duration,
        idle_timeout: core::time::Duration,
    ) {
        self.conn.set_keepalive_us(keepalive.as_micros() as u64);
        self.conn
            .set_idle_timeout_us(idle_timeout.as_micros() as u64);
    }

    /// Feed inbound bytes / collect outbound chunks.
    pub fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        let now_us = self.epoch.elapsed().as_micros() as u64;
        self.conn.pump_at(incoming, now_us).map_err(map_h2)
    }

    /// Dispatch every request whose headers + full body
    /// have arrived.
    pub fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        while let Some(rs) = self.conn.accept_request() {
            self.pending.push(rs);
        }
        let mut dispatched = 0;
        let mut i = 0;
        while i < self.pending.len() {
            let rs = self.pending[i];
            let headers_ready = self.conn.received_headers_ready(&rs);
            let mut probe = Vec::new();
            let (_, end) = self.conn.read_data(&rs, &mut probe);
            // read_data drains — stash what we took.
            if !probe.is_empty() {
                self.bodies.entry(rs.stream_id()).or_default().extend(probe);
            }
            if headers_ready && end {
                let rs = self.pending.swap_remove(i);
                self.handle_one(server, &rs)?;
                dispatched += 1;
            } else {
                i += 1;
            }
        }
        Ok(dispatched)
    }

    fn handle_one(&mut self, server: &Server, rs: &RequestStream) -> Result<(), RpcError> {
        let hdrs = self.conn.read_headers(rs).expect("ready implies Some");
        let body = self.bodies.remove(&rs.stream_id()).unwrap_or_default();
        let meta = match from_h2_headers(&hdrs) {
            Ok(m) => m,
            Err(_) => return self.respond_error(rs, StatusCode::InvalidArgument, "bad headers"),
        };
        let (service, method, user_meta) = match parse_request_headers(&meta) {
            Ok(t) => t,
            Err(_) => {
                return self.respond_error(rs, StatusCode::InvalidArgument, "malformed request");
            }
        };
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

        match crate::dispatch_streaming_deadline(svc, &method, &user_meta, &requests) {
            Ok((responses, trailing)) => {
                self.conn
                    .write_headers(rs, &to_pairs(&encode_response_headers()));
                for resp in responses {
                    let mut wire = Vec::new();
                    encode_frame(
                        &MessageFrame {
                            compressed: false,
                            payload: resp,
                        },
                        &mut wire,
                    );
                    self.conn.write_data(rs, &wire);
                }
                // No `finish` — the trailer HEADERS is the
                // half-close (it queues behind the buffered
                // DATA and carries END_STREAM).
                self.conn.write_trailers(
                    rs,
                    &to_pairs(&encode_trailers(&Status::default(), &trailing)),
                );
            }
            Err(RpcError::Status(status)) => self.respond_status(rs, &status),
            Err(_) => {
                self.respond_error(rs, StatusCode::Internal, "service failure")?;
            }
        }
        self.conn.close_stream(rs);
        Ok(())
    }

    fn respond_status(&mut self, rs: &RequestStream, status: &Status) {
        // Trailers-only: single header block carrying the
        // status, END_STREAM.
        let mut hdrs = encode_response_headers();
        hdrs.headers
            .extend(encode_trailers(status, &Metadata::default()).headers);
        self.conn.write_trailers(rs, &to_pairs(&hdrs));
    }

    fn respond_error(
        &mut self,
        rs: &RequestStream,
        code: StatusCode,
        msg: &str,
    ) -> Result<(), RpcError> {
        self.respond_status(
            rs,
            &Status {
                code,
                message: msg.to_string(),
            },
        );
        Ok(())
    }
}

// ============================================================
// TLS form (T4 — feature `transport-h2-tls`): the same h2
// session over L3's byte-stream TLS 1.3. The TLS wrap is
// a transport middleware — the sans-IO h2 connection
// never knows it's there.
// ============================================================

#[cfg(feature = "transport-h2-tls")]
mod tls {
    use super::{ConnectedChannel, DatagramTransport, H2ClientInner, H2Connection, TcpTransport};
    use crate::{RpcError, Server};
    use alloc::boxed::Box;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use grpcx_crypto::{ClientHandshake, ServerHandshake, TlsConfig, TlsConnection};

    fn map_tls(_e: grpcx_crypto::TlsError) -> RpcError {
        RpcError::Transport("tls: protocol error")
    }

    enum TlsState {
        HandshakingClient(ClientHandshake),
        HandshakingServer(ServerHandshake),
        Open(TlsConnection),
        /// Transient during promotion — never observable
        /// outside `advance`.
        Poisoned,
    }

    /// [`DatagramTransport`] middleware: TLS 1.3 over a
    /// non-blocking TCP stream. The handshake advances
    /// lazily through `send` / `recv` calls (the blocking
    /// facades' drive loop), so neither side ever blocks a
    /// multi-connection accept loop.
    pub struct TlsTransport {
        tcp: TcpTransport,
        state: TlsState,
        /// App data queued while the handshake is still in
        /// flight (e.g. the h2 preface emitted by the first
        /// pump) — flushed on promotion.
        pending_out: Vec<u8>,
        /// Plaintext rustls buffered during the handshake
        /// tail (a peer can pipeline app data right behind
        /// its Finished) — drained ahead of fresh reads.
        inbox: Vec<u8>,
    }

    impl TlsTransport {
        /// Client side: TCP connect + lazy TLS handshake
        /// toward `server_name`. The config should carry
        /// the desired ALPN (gRPC servers expect `h2`).
        pub fn connect(
            addr: &str,
            config: &TlsConfig,
            server_name: &str,
        ) -> Result<Self, RpcError> {
            let tcp = TcpTransport::connect(addr)?;
            let hs = ClientHandshake::new(config, server_name).map_err(map_tls)?;
            Ok(TlsTransport {
                tcp,
                state: TlsState::HandshakingClient(hs),
                pending_out: Vec::new(),
                inbox: Vec::new(),
            })
        }

        /// Server side: wrap an accepted stream.
        pub fn accept_stream(
            stream: std::net::TcpStream,
            config: &TlsConfig,
        ) -> Result<Self, RpcError> {
            let tcp = TcpTransport::from_stream(stream)?;
            let hs = ServerHandshake::new(config).map_err(map_tls)?;
            Ok(TlsTransport {
                tcp,
                state: TlsState::HandshakingServer(hs),
                pending_out: Vec::new(),
                inbox: Vec::new(),
            })
        }

        fn handshaking(&self) -> bool {
            matches!(
                self.state,
                TlsState::HandshakingClient(_) | TlsState::HandshakingServer(_)
            )
        }

        /// One non-blocking handshake step: flush pending
        /// handshake bytes, feed whatever the wire has, and
        /// promote (+ flush queued app data) on completion.
        fn advance(&mut self) -> Result<(), RpcError> {
            loop {
                let (out, complete) = match &mut self.state {
                    TlsState::HandshakingClient(hs) => {
                        (hs.take_handshake_bytes(), hs.is_complete())
                    }
                    TlsState::HandshakingServer(hs) => {
                        (hs.take_handshake_bytes(), hs.is_complete())
                    }
                    _ => return Ok(()),
                };
                if !out.is_empty() {
                    self.tcp.send(&out)?;
                }
                if complete {
                    let conn = match core::mem::replace(&mut self.state, TlsState::Poisoned) {
                        TlsState::HandshakingClient(hs) => hs.into_connection(),
                        TlsState::HandshakingServer(hs) => hs.into_connection(),
                        other => {
                            self.state = other;
                            return Ok(());
                        }
                    }
                    .map_err(map_tls)?;
                    self.state = TlsState::Open(conn);
                    // Drain plaintext the peer pipelined
                    // behind its Finished.
                    if let TlsState::Open(conn) = &mut self.state {
                        let residual = conn.decrypt_app_data(&[]).map_err(map_tls)?;
                        self.inbox.extend_from_slice(&residual);
                    }
                    // Flush app data queued during the
                    // handshake.
                    if !self.pending_out.is_empty() {
                        let queued = core::mem::take(&mut self.pending_out);
                        self.send(&queued)?;
                    }
                    return Ok(());
                }
                match self.tcp.recv()? {
                    Some(chunk) => {
                        let r = match &mut self.state {
                            TlsState::HandshakingClient(hs) => hs.process_handshake_bytes(&chunk),
                            TlsState::HandshakingServer(hs) => hs.process_handshake_bytes(&chunk),
                            _ => Ok(()),
                        };
                        r.map_err(map_tls)?;
                    }
                    None => return Ok(()), // wire idle; resume on next call
                }
            }
        }
    }

    impl DatagramTransport for TlsTransport {
        fn send(&mut self, chunk: &[u8]) -> Result<(), RpcError> {
            if self.handshaking() {
                self.pending_out.extend_from_slice(chunk);
                return self.advance();
            }
            let TlsState::Open(conn) = &mut self.state else {
                return Err(RpcError::Transport("tls: connection poisoned"));
            };
            let wire = conn.encrypt_app_data(chunk).map_err(map_tls)?;
            self.tcp.send(&wire)
        }

        fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
            if self.handshaking() {
                self.advance()?;
                if self.handshaking() {
                    return Ok(None);
                }
            }
            if !self.inbox.is_empty() {
                return Ok(Some(core::mem::take(&mut self.inbox)));
            }
            let TlsState::Open(conn) = &mut self.state else {
                return Err(RpcError::Transport("tls: connection poisoned"));
            };
            match self.tcp.recv()? {
                Some(cipher) => Ok(Some(conn.decrypt_app_data(&cipher).map_err(map_tls)?)),
                None => Ok(None),
            }
        }

        // Park on the underlying TCP socket — correct in
        // every TLS state: handshake progress and app data
        // alike arrive as wire bytes (`advance` flushes
        // pending handshake output before the drive loop
        // ever goes idle, and a non-empty `inbox` means
        // the previous recv progressed, so no park).
        fn wait_readable(&mut self, timeout: core::time::Duration) -> Result<bool, RpcError> {
            self.tcp.wait_readable(timeout)
        }
    }

    impl ConnectedChannel {
        /// Connect to a gRPC-over-h2 (TLS) server at
        /// `addr` (`"host:port"` TCP). `tls` should carry
        /// ALPN `h2` (what gRPC servers negotiate) and the
        /// roots that verify `server_name`.
        pub fn connect_h2(
            addr: &str,
            tls: &TlsConfig,
            server_name: &str,
            authority: &str,
        ) -> Result<Self, RpcError> {
            let transport = TlsTransport::connect(addr, tls, server_name)?;
            let channel = crate::Channel::connect_h2_session(authority);
            Ok(ConnectedChannel::new(channel, Box::new(transport)))
        }
    }

    impl crate::Channel {
        /// Open a sans-IO h2-over-TLS session — the same
        /// H/2 state machine as h2c; TLS lives in the
        /// transport middleware ([`TlsTransport`]).
        pub fn connect_h2_session(authority: &str) -> Self {
            crate::Channel {
                transport: crate::TransportKind::H2,
                #[cfg(feature = "transport-h3")]
                h3: None,
                #[cfg(feature = "transport-ws-mux")]
                wsmux: None,
                h2: Some(H2ClientInner {
                    conn: H2Connection::new_client(),
                    authority: authority.to_string(),
                    epoch: std::time::Instant::now(),
                }),
                #[cfg(feature = "transport-tcp-mux")]
                tcpmux: None,
            }
        }
    }

    impl Server {
        /// Bind a TCP listener and serve gRPC-over-h2
        /// (TLS) until `stop` flips. Per-connection TLS
        /// handshakes advance non-blockingly inside the
        /// shared accept loop.
        pub fn listen_h2_until(
            &self,
            addr: &str,
            tls: &TlsConfig,
            stop: &core::sync::atomic::AtomicBool,
        ) -> Result<(), RpcError> {
            crate::transport_common::serve_tcp_until_with(
                self,
                addr,
                stop,
                |s| s.accept_h2c(),
                |stream| TlsTransport::accept_stream(stream, tls),
            )
        }

        /// [`listen_h2_until`](Self::listen_h2_until)
        /// without a stop flag — serves forever.
        pub fn listen_h2(&self, addr: &str, tls: &TlsConfig) -> Result<(), RpcError> {
            let stop = core::sync::atomic::AtomicBool::new(false);
            self.listen_h2_until(addr, tls, &stop)
        }
    }
}

#[cfg(feature = "transport-h2-tls")]
pub use tls::TlsTransport;
