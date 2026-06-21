#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(
    not(any(
        feature = "transport-h3",
        feature = "transport-h2",
        feature = "transport-tcp-mux",
        feature = "transport-ws-mux"
    )),
    no_std
)]

//! Layout: the public surface is the [`Service`] trait plus
//! a small set of ADTs that model an RPC call ([`Method`] /
//! [`RpcKind`] / [`Status`] / [`StatusCode`] / [`Metadata`]
//! / [`RpcError`]) plus the transport-selection
//! abstractions ([`TransportKind`] / ALPN constants /
//! [`Channel`] / [`Server`]). v0.1 ships the **shape** of
//! every public item with `unimplemented!()` bodies; each
//! v0.x checkpoint replaces a slice with real code.
//!
//! Design intent (per `../../REFACTOR-v10-v0.1-bootstrap.md`):
//!
//! 1. **Compose, don't reimplement.** L10 sits on top of
//!    L4-L9; it doesn't reimplement QUIC / HTTP/3 /
//!    WebSocket / protobuf primitives. The transport
//!    edges land at v0.3+ as dep additions.
//! 2. **ALPN-first transport negotiation.** Non-browser
//!    clients agree on `grpcx/h3` (preferred) or
//!    `grpcx/ws-mux` (fallback) via TLS-ALPN. HTTP/2 is
//!    not in the fallback chain — it is the explicit
//!    **interop bridge** to existing gRPC deployments
//!    (charter revision 2026-06-11): h2c via
//!    `ConnectedChannel::connect_h2c` /
//!    `Server::listen_h2c`, wire-compatible with tonic /
//!    grpc-go / grpc-java.
//! 3. **Browser path** — WebTransport first, then
//!    WebSocket+ws-mux. Servers advertise WT via `Alt-Svc`.
//! 4. **Streaming-first API shape.** All four [`RpcKind`]
//!    variants flow through the same dispatch surface;
//!    unary is just a 1-element stream.
//! 5. `no_std + alloc` for the public ADTs. The runtime
//!    needs `std` for I/O; gated behind a feature at v0.3+.
//! 6. No external runtime deps at v0.1. Transport edges
//!    (`grpcx-quic` / `grpcx-h3` / `grpcx-wt` / `grpcx-ws`
//!    / `grpcx-pb`) land at v0.3+.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[doc(hidden)]
pub mod __fuzz_helpers;
mod framing;
mod headers;
#[cfg(any(feature = "transport-ws-mux", feature = "transport-tcp-mux"))]
mod mux;
mod sizes;
#[cfg(feature = "transport-tcp-mux")]
mod tcpmux;
#[cfg(any(
    feature = "transport-h3",
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
mod transport_common;
#[cfg(feature = "transport-h2")]
mod transport_h2;
#[cfg(feature = "transport-h3")]
mod transport_h3;
#[cfg(feature = "transport-shmem")]
pub mod transport_shmem;
#[cfg(feature = "transport-tcp-mux")]
mod transport_tcp_mux_v2;
#[cfg(feature = "transport-uds")]
mod transport_uds;
pub mod wire_v2;
#[cfg(feature = "transport-ws-mux")]
mod wsmux;

pub use framing::{
    FRAME_HEADER_LEN, MessageFrame, decode_all_frames, decode_frame, encode_frame, try_decode_frame,
};
pub use headers::{
    CONTENT_TYPE_GRPC, GrpcTimeout, TimeoutUnit, USER_AGENT, encode_request_headers,
    encode_response_headers, encode_trailers, parse_request_headers, parse_trailers,
    percent_decode_lossy, percent_encode,
};
#[cfg(any(feature = "transport-ws-mux", feature = "transport-tcp-mux"))]
pub use mux::{
    MUX_DATA, MUX_END_STREAM, MUX_HEADERS, MUX_TRAILERS, decode_header_list, decode_mux_frame,
    encode_header_list, encode_mux_frame,
};
#[cfg(feature = "transport-tcp-mux")]
pub use tcpmux::TcpMuxServerConn;
#[cfg(any(
    feature = "transport-h3",
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-ws-mux"
))]
pub use transport_common::{
    ConnectedChannel, DatagramTransport, TcpTransport, call_bidi, call_client_streaming,
    call_server_streaming, call_unary,
};
#[cfg(feature = "transport-h2")]
pub use transport_h2::H2ServerConn;
#[cfg(feature = "transport-h2-tls")]
pub use transport_h2::TlsTransport;
#[cfg(feature = "transport-h3")]
pub use transport_h3::{
    BidiCall, ClientStreamingCall, ServerConn, ServerStreamingCall, StreamEvent, UdpTransport,
    UnaryCall,
};
#[cfg(feature = "transport-shmem")]
pub use transport_shmem::ShmemConnection;
#[cfg(feature = "transport-tcp-mux")]
pub use transport_tcp_mux_v2::{TcpMuxV2Connection, TcpMuxV2ServerConn};
#[cfg(feature = "transport-uds")]
pub use transport_uds::{UdsConnection, UdsServerConn};
#[cfg(feature = "transport-ws-mux")]
pub use wsmux::{WsMuxServerConn, WsUnaryCall};

// ============================================================
// Service abstraction.
// ============================================================

/// Server-side dispatcher for one logical gRPC service.
///
/// Implementors override [`dispatch`](Self::dispatch) to
/// handle each method by name. The trait is intentionally
/// erased (`&[u8]` in, `Vec<u8>` out) so [`Server::register`]
/// can hold heterogeneous services as boxed trait objects.
/// Registered services must be `Send + Sync` — the
/// threaded TCP serve loop dispatches from worker threads
/// (the tonic contract; stateless impls and `Mutex`-held
/// state satisfy it automatically).
///
/// Typed convenience wrappers (per-method `fn say_hello(req:
/// HelloReq) -> Result<HelloResp, Status>`) are emitted by
/// L8's `.proto` codegen on top of this trait.
pub trait Service {
    /// The fully-qualified service name (e.g.
    /// `"greeter.v1.Greeter"`). Used to route incoming RPCs.
    fn name(&self) -> &str;

    /// Dispatch a single RPC to the named method.
    ///
    /// `request` is the request payload (one protobuf
    /// message for unary, the first frame for streaming);
    /// `metadata` carries the incoming HTTP-shaped headers.
    /// Return `(response_bytes, response_metadata)` on
    /// success or an [`RpcError`] on failure.
    fn dispatch(
        &self,
        method: &str,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError>;

    /// Dispatch an RPC with streaming semantics: the full
    /// sequence of request messages in, a sequence of
    /// response messages out. Covers all four [`RpcKind`]s
    /// — unary is the 1-in/1-out case.
    ///
    /// The transport collects the complete request stream
    /// before calling (batch shape — incremental
    /// backpressure is a v1.x surface), then frames each
    /// returned message in order.
    ///
    /// Default implementation bridges to
    /// [`dispatch`](Self::dispatch): exactly one request
    /// message (or empty) → one response message, which is
    /// what plain unary services want.
    fn dispatch_streaming(
        &self,
        method: &str,
        metadata: &Metadata,
        requests: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        let first = requests.first().map(Vec::as_slice).unwrap_or(&[]);
        let (resp, meta) = self.dispatch(method, metadata, first)?;
        Ok((alloc::vec![resp], meta))
    }

    /// V2 ctx-aware dispatch — the entry point used by
    /// the v2 internal-RPC transports (`transport-uds` /
    /// `transport-shmem`). `ctx` arrives populated with
    /// the incoming request metadata **and** the
    /// kernel-attested [`PeerCred`] (when available);
    /// the handler must place the trailing metadata back
    /// onto `ctx` before returning.
    ///
    /// Default implementation delegates to
    /// [`dispatch_streaming`](Self::dispatch_streaming),
    /// transparently forwarding metadata both directions
    /// and discarding the peer credentials —
    /// appropriate for non-codegen `Service` impls.
    /// Codegen wrappers override this to thread `ctx`
    /// (including `peer_cred`) into the generated
    /// handler call.
    fn dispatch_with_ctx(
        &self,
        method: &str,
        ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        let meta_in = ctx.request_metadata().clone();
        let (resp, trailing) = self.dispatch_streaming(method, &meta_in, requests)?;
        ctx.set_trailing(trailing);
        Ok(resp)
    }
}

// ============================================================
// Transport-agnostic client channel.
// ============================================================

/// Client-side abstraction over an open transport
/// connection. Lets generated `Client` stubs dispatch
/// RPCs without binding to any specific transport — the
/// existing v1 [`ConnectedChannel`] (h2c / h2-tls / h3 /
/// ws-mux / tcp-mux v1) and the v2 [`UdsConnection`] both
/// implement this trait.
///
/// Cross-transport client codegen targets this trait;
/// concrete transports implement it as thin wrappers
/// over their native methods.
#[cfg(any(
    feature = "transport-h3",
    feature = "transport-ws-mux",
    feature = "transport-h2",
    feature = "transport-tcp-mux",
    feature = "transport-uds"
))]
pub trait RpcChannel {
    /// Issue a unary RPC. Returns the response bytes and
    /// trailing metadata, or an [`RpcError`].
    fn call_unary(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError>;

    /// Issue a server-streaming RPC.
    fn call_server_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError>;

    /// Issue a client-streaming RPC.
    fn call_client_streaming(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<u8>, Metadata), RpcError>;

    /// Issue a bidirectional-streaming RPC.
    fn call_bidi(
        &mut self,
        method: &Method,
        metadata: &Metadata,
        requests: &[&[u8]],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError>;
}

/// Per-RPC metadata identifying the service + method +
/// shape.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Method {
    /// Fully-qualified service name (e.g.
    /// `"greeter.v1.Greeter"`).
    pub service: String,
    /// Method name within the service (e.g. `"SayHello"`).
    pub method: String,
    /// RPC shape — see [`RpcKind`].
    pub kind: RpcKind,
}

/// gRPC RPC shape — every gRPC call falls into one of
/// these four buckets.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RpcKind {
    /// One request, one response.
    #[default]
    Unary,
    /// One request, stream of responses.
    ServerStreaming,
    /// Stream of requests, one response.
    ClientStreaming,
    /// Stream of requests, stream of responses.
    BidirectionalStreaming,
}

// ============================================================
// Status.
// ============================================================

/// gRPC RPC status — a `(code, human-readable message)`
/// pair carried in response trailers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Status {
    /// The numeric status code per the gRPC spec.
    pub code: StatusCode,
    /// Optional human-readable status message.
    pub message: String,
}

/// gRPC standard status codes (numeric values match the
/// spec; see `SPEC.md` for the table).
///
/// Repr-default so the discriminant equals the spec
/// number (`Ok = 0`, `Cancelled = 1`, …,
/// `Unauthenticated = 16`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum StatusCode {
    /// `0` — no error.
    #[default]
    Ok = 0,
    /// `1` — the operation was cancelled.
    Cancelled = 1,
    /// `2` — unknown error.
    Unknown = 2,
    /// `3` — caller specified an invalid argument.
    InvalidArgument = 3,
    /// `4` — deadline expired before the operation
    /// completed.
    DeadlineExceeded = 4,
    /// `5` — requested entity not found.
    NotFound = 5,
    /// `6` — entity that a client attempted to create
    /// already exists.
    AlreadyExists = 6,
    /// `7` — caller does not have permission.
    PermissionDenied = 7,
    /// `8` — a resource has been exhausted (quota, etc.).
    ResourceExhausted = 8,
    /// `9` — operation rejected because the system is not
    /// in a state required for the operation's execution.
    FailedPrecondition = 9,
    /// `10` — operation aborted (typically due to a
    /// concurrency issue).
    Aborted = 10,
    /// `11` — operation attempted past the valid range.
    OutOfRange = 11,
    /// `12` — operation is not implemented or supported.
    Unimplemented = 12,
    /// `13` — internal errors.
    Internal = 13,
    /// `14` — service unavailable.
    Unavailable = 14,
    /// `15` — unrecoverable data loss or corruption.
    DataLoss = 15,
    /// `16` — request lacks valid authentication
    /// credentials.
    Unauthenticated = 16,
}

impl StatusCode {
    /// Map a numeric `grpc-status` value back to the
    /// enum. Returns `None` outside `0..=16` (the gRPC
    /// spec reserves nothing else; unknown codes are a
    /// peer bug surfaced as a codec error by
    /// [`parse_trailers`]).
    pub fn from_i32(n: i32) -> Option<Self> {
        Some(match n {
            0 => StatusCode::Ok,
            1 => StatusCode::Cancelled,
            2 => StatusCode::Unknown,
            3 => StatusCode::InvalidArgument,
            4 => StatusCode::DeadlineExceeded,
            5 => StatusCode::NotFound,
            6 => StatusCode::AlreadyExists,
            7 => StatusCode::PermissionDenied,
            8 => StatusCode::ResourceExhausted,
            9 => StatusCode::FailedPrecondition,
            10 => StatusCode::Aborted,
            11 => StatusCode::OutOfRange,
            12 => StatusCode::Unimplemented,
            13 => StatusCode::Internal,
            14 => StatusCode::Unavailable,
            15 => StatusCode::DataLoss,
            16 => StatusCode::Unauthenticated,
            _ => return None,
        })
    }
}

/// HTTP-shaped header bag carried alongside an RPC.
/// Headers may carry binary values (the `-bin` suffix
/// convention from gRPC; v1.x adds typed wrappers).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// `(name, value)` pairs.
    pub headers: Vec<(String, Vec<u8>)>,
}

/// Kernel-attested credentials of the connection peer.
///
/// Populated for the `transport-uds` / `transport-shmem`
/// transports — where the peer side of the AF_UNIX
/// rendezvous socket has a kernel-known `uid` / `gid` /
/// `pid` — and left `None` for `transport-tcp-mux` /
/// `transport-h2c` / `transport-h2-tls` / `transport-h3`
/// where no kernel-attested identity exists.
///
/// `gid` and `pid` are platform-dependent: Linux's
/// `SO_PEERCRED` yields all three; Darwin / FreeBSD's
/// `getpeereid(3)` yields `uid` + `gid` but not `pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    /// Kernel-attested effective uid of the peer.
    pub uid: u32,
    /// Kernel-attested effective gid of the peer, if the
    /// platform provides it.
    pub gid: Option<u32>,
    /// Kernel-attested pid of the peer, if the platform
    /// provides it (Linux yes; Darwin / FreeBSD no).
    pub pid: Option<i32>,
}

/// Per-call server-side context — the request metadata
/// in, the trailing metadata out, plus a typed slot for
/// kernel-attested [`PeerCred`] when the transport
/// supplies one. The repair for tonic's
/// `Request<T>`/`Response<T>` wrapper noise (docs/dx-scope.md
/// § P2): generated handlers take the message directly
/// plus `&mut CallContext`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CallContext {
    request: Metadata,
    trailing: Metadata,
    peer_cred: Option<PeerCred>,
}

impl CallContext {
    /// Wrap the incoming request metadata.
    pub fn new(request: Metadata) -> Self {
        CallContext {
            request,
            trailing: Metadata::default(),
            peer_cred: None,
        }
    }

    /// The caller's request metadata (headers).
    pub fn request_metadata(&self) -> &Metadata {
        &self.request
    }

    /// Append one trailing-metadata entry to send back
    /// with the response.
    pub fn add_trailing(&mut self, name: &str, value: &[u8]) {
        self.trailing
            .headers
            .push((String::from(name), value.to_vec()));
    }

    /// Consume the context, yielding the trailing
    /// metadata for the response.
    pub fn into_trailing(self) -> Metadata {
        self.trailing
    }

    /// The trailing metadata received with the response
    /// (client side, populated by generated client fns
    /// after the call completes).
    pub fn trailing(&self) -> &Metadata {
        &self.trailing
    }

    /// Append one request-metadata entry to send with the
    /// call (client side).
    pub fn add_request_header(&mut self, name: &str, value: &[u8]) {
        self.request
            .headers
            .push((String::from(name), value.to_vec()));
    }

    /// Bound this call: sets the standard `grpc-timeout`
    /// request header (PROTOCOL-HTTP2 wire format). The
    /// blocking facades enforce it locally — the call
    /// returns `Status(DEADLINE_EXCEEDED)` once the
    /// deadline passes (connection establishment included,
    /// per gRPC semantics) — and the server refuses to
    /// ship a response whose handler finished after the
    /// deadline. The connection itself stays usable
    /// (a deadline is a call verdict; link death is the
    /// transport's liveness verdict).
    pub fn set_timeout(&mut self, timeout: core::time::Duration) {
        self.request.headers.retain(|(n, _)| n != "grpc-timeout");
        self.request
            .headers
            .push(GrpcTimeout::from_duration(timeout).to_header());
    }

    /// Replace the trailing metadata. Transport plumbing —
    /// generated client fns call this after the response
    /// arrives; handlers should use
    /// [`add_trailing`](Self::add_trailing) instead.
    pub fn set_trailing(&mut self, trailing: Metadata) {
        self.trailing = trailing;
    }

    /// Kernel-attested credentials of the connection
    /// peer, when the transport supplies them
    /// (`transport-uds` / `transport-shmem`). `None` on
    /// the network transports (`transport-tcp-mux` /
    /// `transport-h2c` / `transport-h2-tls` /
    /// `transport-h3`).
    pub fn peer_cred(&self) -> Option<&PeerCred> {
        self.peer_cred.as_ref()
    }

    /// Transport plumbing — populated by the server-side
    /// accept path on UDS / shmem before the handler
    /// runs. Application handlers must not call this.
    pub fn set_peer_cred(&mut self, peer_cred: Option<PeerCred>) {
        self.peer_cred = peer_cred;
    }
}

// ============================================================
// Errors.
// ============================================================

/// Failure modes raised by client and server runtimes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// RPC-level failure with a gRPC status code +
    /// optional message (e.g.
    /// `Status { code: NotFound, message: "..." }`).
    Status(Status),
    /// Transport-layer failure (e.g. connection refused,
    /// stream reset). The static string identifies the
    /// specific failure mode; v1.x may expand the variant.
    Transport(&'static str),
    /// Codec failure (protobuf encode/decode error).
    Codec(&'static str),
    /// No transport from [`TransportKind`] is available
    /// for the call (e.g. ALPN negotiation found no
    /// common token).
    TransportUnavailable,
}

// ============================================================
// Transport negotiation.
// ============================================================

/// Enumerates the wire transports L10 can run over.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// HTTP/3 over QUIC. Preferred for non-browser
    /// clients. ALPN token: [`ALPN_H3`].
    #[default]
    H3,
    /// WebSocket with application-level multiplexing.
    /// Fallback for non-browser clients. ALPN token:
    /// [`ALPN_WS_MUX`].
    WsMux,
    /// WebTransport over HTTP/3. Browser path; advertised
    /// via `Alt-Svc`. No dedicated ALPN token — the
    /// underlying H/3 connection carries it.
    WebTransport,
    /// HTTP/2 cleartext (h2c, prior knowledge) over TCP —
    /// the interop bridge to existing gRPC deployments
    /// (charter 2026-06-11).
    H2c,
    /// HTTP/2 over TLS 1.3 (T4) — the same H/2 session as
    /// [`H2c`](Self::H2c) with L3's byte-stream TLS as
    /// transport middleware. ALPN `h2` by convention
    /// (caller-configured).
    H2,
    /// The ws-mux frame grammar over raw TCP with
    /// length-prefixed records — bare-metal links where
    /// neither TLS nor HTTP framing is wanted (T7,
    /// charter 2026-06-11). Prior knowledge; no
    /// handshake.
    TcpMux,
}

/// ALPN token for the HTTP/3 + QUIC transport
/// (preferred).
pub const ALPN_H3: &str = "grpcx/h3";

/// ALPN token for the WebSocket multiplex transport
/// (fallback).
pub const ALPN_WS_MUX: &str = "grpcx/ws-mux";

// ============================================================
// Client.
// ============================================================

/// Client-side handle to an open transport connection.
///
/// `Channel` holds the negotiated transport + connection
/// state; one channel can carry many concurrent RPCs.
///
/// With the `transport-h3` feature (default), construct
/// via [`Channel::connect_h3`] and drive sans-IO with
/// [`Channel::pump`] / the blocking [`call_unary`] facade.
pub struct Channel {
    transport: TransportKind,
    #[cfg(feature = "transport-h3")]
    h3: Option<transport_h3::H3ClientInner>,
    #[cfg(feature = "transport-ws-mux")]
    wsmux: Option<wsmux::WsMuxClient>,
    #[cfg(feature = "transport-h2")]
    h2: Option<transport_h2::H2ClientInner>,
    #[cfg(feature = "transport-tcp-mux")]
    tcpmux: Option<tcpmux::TcpMuxClient>,
}

impl core::fmt::Debug for Channel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct("Channel");
        d.field("transport", &self.transport);
        #[cfg(feature = "transport-h3")]
        d.field("h3", &self.h3.is_some());
        d.finish()
    }
}

impl Channel {
    /// Open a connection to `addr` over `transport`,
    /// resolving + binding OS sockets.
    ///
    /// # v0.x implementation status
    ///
    /// Still `unimplemented!()` — socket-backed connect
    /// arrives with the L1/L2 runtime wiring (v0.6+). The
    /// sans-IO path is live today:
    /// [`Channel::connect_h3`] + a [`DatagramTransport`]
    /// of your choosing.
    pub fn connect(_addr: &str, _transport: TransportKind) -> Result<Self, RpcError> {
        unimplemented!("use ConnectedChannel::connect_h3(addr, tls, server_name)")
    }

    /// The transport this channel is negotiated for.
    pub fn transport(&self) -> TransportKind {
        self.transport
    }
}

// ============================================================
// grpc-timeout plumbing (codec = headers::GrpcTimeout).
// ============================================================

/// A request's `grpc-timeout`, if present and well-formed.
#[cfg(any(
    feature = "transport-h3",
    feature = "transport-ws-mux",
    feature = "transport-h2",
    feature = "transport-tcp-mux"
))]
pub(crate) fn grpc_timeout_of(meta: &Metadata) -> Option<core::time::Duration> {
    meta.headers
        .iter()
        .find(|(n, _)| n == "grpc-timeout")
        .and_then(|(_, v)| GrpcTimeout::parse(v))
        .map(GrpcTimeout::as_duration)
}

/// Server-side deadline honesty: dispatch, but refuse to
/// ship a response whose handler finished after the
/// peer's `grpc-timeout` already expired — the client gave
/// up; a stale success would be a lie. (The handler itself
/// is synchronous and cannot be interrupted — same
/// constraint as any blocking-handler gRPC server.)
#[cfg(any(
    feature = "transport-h3",
    feature = "transport-ws-mux",
    feature = "transport-h2",
    feature = "transport-tcp-mux"
))]
pub(crate) fn dispatch_streaming_deadline(
    svc: &(dyn Service + Send + Sync),
    method: &str,
    meta: &Metadata,
    requests: &[alloc::vec::Vec<u8>],
) -> Result<(alloc::vec::Vec<alloc::vec::Vec<u8>>, Metadata), RpcError> {
    let timeout = grpc_timeout_of(meta);
    let t0 = timeout.map(|_| std::time::Instant::now());
    let r = svc.dispatch_streaming(method, meta, requests);
    if let (Some(t), Some(t0)) = (timeout, t0)
        && t0.elapsed() > t
    {
        return Err(RpcError::Status(Status {
            code: StatusCode::DeadlineExceeded,
            message: String::from("deadline exceeded before the handler returned"),
        }));
    }
    r
}

// ============================================================
// Server.
// ============================================================

/// Server-side dispatcher that owns a set of [`Service`]
/// trait objects and accepts incoming connections.
///
/// Cloning is cheap and shares the service registry (the
/// tonic-`Router` shape) — the threaded TCP serve loop
/// hands each connection task its own `Server` clone.
#[derive(Default, Clone)]
pub struct Server {
    services: alloc::sync::Arc<Vec<Box<dyn Service + Send + Sync>>>,
    /// Passive idle GC (0.25): a connection with no
    /// inbound traffic for this long is reaped by its
    /// serve task. `None` (default) = never — the
    /// server-side-liveness-off decision stands; this is
    /// reaping, not probing.
    max_connection_idle: Option<core::time::Duration>,
}

impl Server {
    /// Construct a fresh server with no registered
    /// services.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opt into passive idle-connection GC: serve-loop
    /// connections with no inbound traffic for `idle` are
    /// reaped (their task exits, state drops). The
    /// blocking client does NOT auto-reconnect — a reaped
    /// client's next call fails and the caller
    /// re-establishes. `None` (default) keeps connections
    /// forever.
    pub fn set_max_connection_idle(&mut self, idle: Option<core::time::Duration>) {
        self.max_connection_idle = idle;
    }

    pub(crate) fn max_connection_idle(&self) -> Option<core::time::Duration> {
        self.max_connection_idle
    }

    /// Register a service. The server walks the
    /// registered services' [`Service::name`]s on each
    /// incoming RPC to find the dispatcher.
    ///
    /// # Panics
    ///
    /// Panics if called while a serve loop is running (the
    /// registry is shared with live connection tasks).
    /// Register every service first, then serve; once all
    /// serve loops have stopped, registration re-opens.
    pub fn register(&mut self, svc: Box<dyn Service + Send + Sync>) {
        alloc::sync::Arc::get_mut(&mut self.services)
            .expect("grpcx: register while a serve loop is running")
            .push(svc);
    }

    /// Bind to `addr` and serve registered services over
    /// `transport`. Blocks until the server shuts down.
    ///
    /// # v0.x implementation status
    ///
    /// v0.1 ships the API surface only — this function
    /// panics with `unimplemented!()`. Per-transport
    /// accept loops land at v0.3 (`grpcx/h3`), v0.6
    /// (`grpcx/ws-mux`).
    pub fn listen(&self, _addr: &str, _transport: TransportKind) -> Result<(), RpcError> {
        unimplemented!("use Server::listen_h3(addr, tls) / listen_h3_until")
    }

    /// Number of registered services. Exposed so callers
    /// can sanity-check their service-registration phase.
    pub fn service_count(&self) -> usize {
        self.services.len()
    }

    /// Route an incoming RPC's service name to its
    /// registered dispatcher.
    #[cfg(any(
        feature = "transport-h3",
        feature = "transport-ws-mux",
        feature = "transport-h2",
        feature = "transport-tcp-mux",
        feature = "transport-uds"
    ))]
    pub(crate) fn find_service(&self, name: &str) -> Option<&(dyn Service + Send + Sync)> {
        self.services
            .iter()
            .map(Box::as_ref)
            .find(|s| s.name() == name)
    }
}

// ============================================================
// ALPN negotiation + Alt-Svc (pure functions — SPEC.md
// § ALPN negotiation strategy).
// ============================================================

/// Pick the transport for a connection given both sides'
/// ALPN offers: `grpcx/h3` preferred, `grpcx/ws-mux`
/// fallback, `None` when no token matches (HTTP/2 is NOT
/// a fallback target per the charter).
pub fn select_alpn_transport(client: &[&str], server: &[&str]) -> Option<TransportKind> {
    let both = |tok: &str| client.contains(&tok) && server.contains(&tok);
    if both(ALPN_H3) {
        Some(TransportKind::H3)
    } else if both(ALPN_WS_MUX) {
        Some(TransportKind::WsMux)
    } else {
        None
    }
}

/// Render the `Alt-Svc` header advertising the
/// WebTransport/H3 endpoint to browsers (SPEC.md):
/// `h3-grpcx=":4433"; ma=86400`.
pub fn build_alt_svc_header(port: u16, max_age_secs: u32) -> String {
    let mut s = String::from("h3-grpcx=\":");
    push_u32(&mut s, u32::from(port));
    s.push_str("\"; ma=");
    push_u32(&mut s, max_age_secs);
    s
}

/// Parse the port out of an `Alt-Svc` header produced by
/// [`build_alt_svc_header`] (or any `h3-grpcx=":PORT"`
/// form). `None` when the token is absent or malformed.
pub fn parse_alt_svc_header(value: &str) -> Option<u16> {
    let rest = value.split("h3-grpcx=\":").nth(1)?;
    let port_str = rest.split('"').next()?;
    port_str.parse().ok()
}

fn push_u32(s: &mut String, mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &b in &buf[i..] {
        s.push(b as char);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(
        feature = "transport-h3",
        feature = "transport-ws-mux",
        feature = "transport-h2",
        feature = "transport-tcp-mux"
    ))]
    #[test]
    fn server_refuses_stale_success_past_deadline() {
        use super::*;
        struct Slow;
        impl Service for Slow {
            fn name(&self) -> &str {
                "slow"
            }
            fn dispatch(
                &self,
                _m: &str,
                _md: &Metadata,
                r: &[u8],
            ) -> Result<(Vec<u8>, Metadata), RpcError> {
                std::thread::sleep(core::time::Duration::from_millis(20));
                Ok((r.to_vec(), Metadata::default()))
            }
        }
        let meta = Metadata {
            headers: alloc::vec![(
                String::from("grpc-timeout"),
                b"5m".to_vec() // 5 ms — the 20 ms handler overruns it
            )],
        };
        let r = dispatch_streaming_deadline(&Slow, "M", &meta, &[]);
        assert!(
            matches!(
                r,
                Err(RpcError::Status(Status {
                    code: StatusCode::DeadlineExceeded,
                    ..
                }))
            ),
            "got {r:?}"
        );
        // Without the header, the same handler's response ships.
        let r = dispatch_streaming_deadline(&Slow, "M", &Metadata::default(), &[]);
        assert!(r.is_ok());
    }

    use super::*;
    use core::mem::size_of;

    #[test]
    fn error_is_compact() {
        // `RpcError` carries either a `Status` (the wide
        // variant: `(StatusCode, String)`) or a small
        // payload. The 96-byte bound accommodates a 4-byte
        // discriminant + 24-byte `Status` header padding
        // + 24-byte `String`-shape inner.
        assert!(size_of::<RpcError>() <= 96);
    }

    #[test]
    fn status_code_numerics_match_spec() {
        // gRPC standard codes 0..=16. Each variant's
        // numeric value (after `as i32`) must equal the
        // spec value.
        assert_eq!(StatusCode::Ok as i32, 0);
        assert_eq!(StatusCode::Cancelled as i32, 1);
        assert_eq!(StatusCode::Unknown as i32, 2);
        assert_eq!(StatusCode::InvalidArgument as i32, 3);
        assert_eq!(StatusCode::DeadlineExceeded as i32, 4);
        assert_eq!(StatusCode::NotFound as i32, 5);
        assert_eq!(StatusCode::AlreadyExists as i32, 6);
        assert_eq!(StatusCode::PermissionDenied as i32, 7);
        assert_eq!(StatusCode::ResourceExhausted as i32, 8);
        assert_eq!(StatusCode::FailedPrecondition as i32, 9);
        assert_eq!(StatusCode::Aborted as i32, 10);
        assert_eq!(StatusCode::OutOfRange as i32, 11);
        assert_eq!(StatusCode::Unimplemented as i32, 12);
        assert_eq!(StatusCode::Internal as i32, 13);
        assert_eq!(StatusCode::Unavailable as i32, 14);
        assert_eq!(StatusCode::DataLoss as i32, 15);
        assert_eq!(StatusCode::Unauthenticated as i32, 16);
    }

    #[test]
    fn alpn_constants() {
        // The ALPN tokens are wire-visible; a typo would
        // break interop. Lock them.
        assert_eq!(ALPN_H3, "grpcx/h3");
        assert_eq!(ALPN_WS_MUX, "grpcx/ws-mux");
    }
}
