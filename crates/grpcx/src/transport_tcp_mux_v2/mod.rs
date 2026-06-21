//! `transport-tcp-mux v2` — raw-TCP same-network
//! transport carrying wire-format v2 frames (no
//! length-prefix shell, no mux framing — the v2 16-byte
//! header is the only framing on the wire).
//!
//! Public types: [`TcpMuxV2Connection`] (client side) and
//! [`TcpMuxV2ServerConn`] (server-side accepted connection).
//! Server-side serve loops are entered via
//! `Server::listen_tcp_mux_v2` /
//! `Server::listen_tcp_mux_v2_until`
//! (see [`crate::Server`]).
//!
//! Parallels [`crate::transport_uds`] one-for-one: the
//! frame state machine, the RpcChannel impl, the
//! per-connection serve loop, and the writev primitives
//! are all identical — the only differences are the
//! socket type ([`std::net::TcpStream`] in place of
//! `UnixStream`), the absence of kernel-attested
//! [`crate::PeerCred`], and the address shape
//! (`"host:port"` strings in place of filesystem paths).
//!
//! See `_dev/RFC-internal-rpc-arch.md` § 5 for the
//! design rationale and `_dev/SPEC-wire-v2.md` for the
//! normative wire grammar.

pub(crate) mod connection;
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
pub(crate) mod linux_iouring;
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
pub(crate) mod linux_iouring_client;
#[cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]
pub(crate) mod linux_iouring_v2;
pub(crate) mod server;
pub(crate) mod wire;

pub use connection::TcpMuxV2Connection;
pub use server::TcpMuxV2ServerConn;
