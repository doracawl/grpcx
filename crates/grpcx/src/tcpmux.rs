// `grpcx/tcp-mux` transport — T7 (charter 2026-06-11):
// the v0.6 ws-mux shed of its WebSocket shell, for
// bare-metal links where neither TLS nor HTTP framing is
// wanted. Same mux frame grammar (`mux.rs`), but the
// outer shell is a `u32-BE length || mux-frame` record
// stream over raw TCP — no upgrade handshake, no masking:
// the connection is ready the moment it opens (prior
// knowledge, like h2c).
//
// API mirrors the other transports: `ConnectedChannel::
// connect_tcp_mux(addr, authority)` / `Server::
// listen_tcp_mux(addr)`; generated stubs unchanged.

use crate::mux::{
    MUX_DATA, MUX_HEADERS, RecvStream, dispatch_one_mux, encode_header_list, encode_mux_frame,
    poll_completed_mux, route_mux_frame,
};
use crate::transport_common::{ConnectedChannel, DatagramTransport, TcpTransport, drive_once};
use crate::{
    MessageFrame, Metadata, Method, RpcError, Server, encode_frame, encode_request_headers,
};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ============================================================
// Length-prefixed record shell.
// ============================================================

/// Sanity cap on one record (a mux frame carrying one
/// gRPC message section). Larger declared lengths are a
/// peer violation, not a pending state.
const MAX_RECORD: usize = 64 * 1024 * 1024;

fn push_record(out_queue: &mut Vec<Vec<u8>>, mux: Vec<u8>) {
    let mut rec = Vec::with_capacity(4 + mux.len());
    rec.extend_from_slice(&(mux.len() as u32).to_be_bytes());
    rec.extend_from_slice(&mux);
    out_queue.push(rec);
}

/// Drain complete records out of `in_buf` and route the
/// mux frames into `streams`.
fn drain_records(
    in_buf: &mut Vec<u8>,
    streams: &mut BTreeMap<u32, RecvStream>,
    out_queue: &mut Vec<Vec<u8>>,
) -> Result<(), RpcError> {
    loop {
        if in_buf.len() < 4 {
            return Ok(());
        }
        let len = u32::from_be_bytes([in_buf[0], in_buf[1], in_buf[2], in_buf[3]]) as usize;
        if len > MAX_RECORD {
            return Err(RpcError::Transport("tcp-mux: record exceeds size cap"));
        }
        if in_buf.len() < 4 + len {
            return Ok(());
        }
        let rec: Vec<u8> = in_buf.drain(..4 + len).skip(4).collect();
        if let Some(pong) = route_mux_frame(streams, &rec)? {
            push_record(out_queue, pong);
        }
    }
}

// ============================================================
// Client.
// ============================================================

/// Client side of a `grpcx/tcp-mux` connection (sans-IO).
pub(crate) struct TcpMuxClient {
    authority: String,
    in_buf: Vec<u8>,
    out_queue: Vec<Vec<u8>>,
    streams: BTreeMap<u32, RecvStream>,
    next_stream_id: u32,
    liveness: crate::mux::LinkLiveness,
}

impl TcpMuxClient {
    pub(crate) fn new(authority: &str) -> Self {
        TcpMuxClient {
            authority: authority.to_string(),
            in_buf: Vec::new(),
            out_queue: Vec::new(),
            streams: BTreeMap::new(),
            next_stream_id: 1,
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

    /// No handshake — ready from the first byte.
    pub(crate) fn is_ready(&self) -> bool {
        true
    }

    pub(crate) fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        if let Some(bytes) = incoming
            && !bytes.is_empty()
        {
            self.liveness.on_inbound();
            self.in_buf.extend_from_slice(bytes);
        }
        drain_records(&mut self.in_buf, &mut self.streams, &mut self.out_queue)?;
        if self.liveness.check(true, "tcp-mux: link dead")? {
            push_record(
                &mut self.out_queue,
                encode_mux_frame(0, crate::mux::MUX_PING, false, &[]),
            );
        }
        Ok(core::mem::take(&mut self.out_queue))
    }

    /// Open a stream: HEADERS, then one DATA record per
    /// gRPC frame, END_STREAM on the last (or an empty
    /// END_STREAM DATA when there are no requests).
    fn open_streaming(&mut self, headers: &Metadata, grpc_frames: &[Vec<u8>]) -> u32 {
        let sid = self.next_stream_id;
        self.next_stream_id += 2; // odd ids, client side
        push_record(
            &mut self.out_queue,
            encode_mux_frame(sid, MUX_HEADERS, false, &encode_header_list(headers)),
        );
        if grpc_frames.is_empty() {
            push_record(
                &mut self.out_queue,
                encode_mux_frame(sid, MUX_DATA, true, &[]),
            );
        } else {
            for (i, frame) in grpc_frames.iter().enumerate() {
                let last = i + 1 == grpc_frames.len();
                push_record(
                    &mut self.out_queue,
                    encode_mux_frame(sid, MUX_DATA, last, frame),
                );
            }
        }
        self.streams.insert(sid, RecvStream::default());
        sid
    }

    /// Eager cancel (0.22): MUX_RST for the sid + local
    /// state dropped; flushed by the next pump.
    fn cancel(&mut self, sid: u32) {
        push_record(
            &mut self.out_queue,
            encode_mux_frame(sid, crate::mux::MUX_RST, false, &[]),
        );
        self.streams.remove(&sid);
    }
}

impl crate::Channel {
    /// Open a sans-IO `grpcx/tcp-mux` session. No
    /// handshake: the channel is ready immediately.
    pub fn connect_tcp_mux_session(authority: &str) -> Self {
        crate::Channel {
            transport: crate::TransportKind::TcpMux,
            #[cfg(feature = "transport-h3")]
            h3: None,
            #[cfg(feature = "transport-ws-mux")]
            wsmux: None,
            #[cfg(feature = "transport-h2")]
            h2: None,
            tcpmux: Some(TcpMuxClient::new(authority)),
        }
    }
}

/// The streaming core every tcp-mux facade rides: all
/// request messages out (batch shape), every response
/// message collected until the stream completes.
pub(crate) fn call_streaming_tcp_mux(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    requests: &[&[u8]],
) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
    let inner = channel
        .tcpmux
        .as_mut()
        .ok_or(RpcError::TransportUnavailable)?;
    let authority = inner.authority.clone();
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

    let deadline = crate::transport_common::CallDeadline::from_metadata(metadata);
    let mut idle = 0u32;
    loop {
        if let Err(e) = deadline.check() {
            // Eager cancel (0.22): MUX_RST + best-effort
            // flush, then the verdict.
            let inner = channel.tcpmux.as_mut().expect("checked above");
            inner.cancel(sid);
            if let Ok(outs) = channel.pump(None) {
                for out in outs {
                    let _ = transport.send(&out);
                }
            }
            return Err(e);
        }
        let inner = channel.tcpmux.as_mut().expect("checked above");
        if let Some(done) = poll_completed_mux(&mut inner.streams, sid)? {
            return Ok(done);
        }
        idle = drive_once(channel, transport, idle)?;
    }
}

/// Blocking unary over tcp-mux — dispatched from the
/// shared `call_unary`.
pub(crate) fn call_unary_tcp_mux(
    channel: &mut crate::Channel,
    transport: &mut dyn DatagramTransport,
    method: &Method,
    metadata: &Metadata,
    request: &[u8],
) -> Result<(Vec<u8>, Metadata), RpcError> {
    let (msgs, trailing) =
        call_streaming_tcp_mux(channel, transport, method, metadata, &[request])?;
    Ok((crate::transport_common::first_or_empty(msgs), trailing))
}

impl ConnectedChannel {
    /// Connect to a `grpcx/tcp-mux` server at `addr`
    /// (`"host:port"` raw TCP — no TLS, no HTTP, no
    /// handshake).
    pub fn connect_tcp_mux(addr: &str, authority: &str) -> Result<Self, RpcError> {
        let transport = TcpTransport::connect(addr)?;
        let channel = crate::Channel::connect_tcp_mux_session(authority);
        Ok(ConnectedChannel::new(channel, Box::new(transport)))
    }
}

// ============================================================
// Server.
// ============================================================

/// Server side of one `grpcx/tcp-mux` connection
/// (sans-IO). Constructed by [`Server::accept_tcp_mux`],
/// driven with [`pump`](Self::pump) +
/// [`dispatch`](Self::dispatch).
pub struct TcpMuxServerConn {
    in_buf: Vec<u8>,
    out_queue: Vec<Vec<u8>>,
    streams: BTreeMap<u32, RecvStream>,
    liveness: crate::mux::LinkLiveness,
}

impl Server {
    /// Create the server side of a tcp-mux connection.
    pub fn accept_tcp_mux(&self) -> TcpMuxServerConn {
        let mut liveness = crate::mux::LinkLiveness::new();
        // Server-side liveness OFF by default — see
        // `Server::accept_h3`.
        liveness.configure(core::time::Duration::MAX, core::time::Duration::MAX);
        TcpMuxServerConn {
            in_buf: Vec::new(),
            out_queue: Vec::new(),
            streams: BTreeMap::new(),
            liveness,
        }
    }

    /// Bind a TCP listener and serve `grpcx/tcp-mux` until
    /// `stop` flips. Single-threaded multi-connection
    /// (same bootstrap-parity shape as the other
    /// transports).
    pub fn listen_tcp_mux_until(
        &self,
        addr: &str,
        stop: &core::sync::atomic::AtomicBool,
    ) -> Result<(), RpcError> {
        crate::transport_common::serve_tcp_until(self, addr, stop, |s| s.accept_tcp_mux())
    }

    /// [`listen_tcp_mux_until`](Self::listen_tcp_mux_until)
    /// without a stop flag — serves forever.
    pub fn listen_tcp_mux(&self, addr: &str) -> Result<(), RpcError> {
        let stop = core::sync::atomic::AtomicBool::new(false);
        self.listen_tcp_mux_until(addr, &stop)
    }
}

impl TcpMuxServerConn {
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
        drain_records(&mut self.in_buf, &mut self.streams, &mut self.out_queue)?;
        if self.liveness.check(true, "tcp-mux: link dead")? {
            push_record(
                &mut self.out_queue,
                encode_mux_frame(0, crate::mux::MUX_PING, false, &[]),
            );
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
                push_record(&mut self.out_queue, mux);
            }
            dispatched += 1;
        }
        Ok(dispatched)
    }
}

impl crate::transport_common::TcpServeConn for TcpMuxServerConn {
    fn pump(&mut self, incoming: Option<&[u8]>) -> Result<Vec<Vec<u8>>, RpcError> {
        TcpMuxServerConn::pump(self, incoming)
    }
    fn dispatch(&mut self, server: &Server) -> Result<usize, RpcError> {
        TcpMuxServerConn::dispatch(self, server)
    }
}
