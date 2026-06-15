#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Layout: the working public surface is [`QuicConnection`] —
//! the v0.6+ Connection state machine that drives a complete
//! QUIC handshake to 1-RTT and multiplexes bidirectional streams.
//! Lower-level building blocks ([`Frame`] / [`PacketHeader`] /
//! [`AeadSuite`] / [`build_initial_packet`] etc.) are also
//! exported for callers who want to operate at the wire-format
//! layer (HTTP/3 framing in L5, or test harnesses).
//!
//! The original v0.1 `Endpoint` / `Connecting` / stream-handle
//! surface was an aspirational sketch; the v0.6+ realisation
//! collapses it into the single [`QuicConnection`] driver. The
//! pump-shaped API (`pump(incoming) -> outbound datagrams`)
//! suits the in-memory loopback bus today and the future
//! `grpcx-io` UDP socket binding equally well.
//!
//! Design intent (per
//! `../../REFACTOR-v4-v0.1-bootstrap.md`):
//!
//! 1. Zero-alloc on the packet hot path (caller-provided buffers
//!    on the wire-format layer; v1.x adds borrow-only `FrameRef`).
//! 2. Slab pool for connections + streams (v1.x; today
//!    [`QuicConnection`] uses a `BTreeMap<u64, _>` for clarity).
//! 3. Reuse L2's `Sleep` / `Interval` for PTO + idle + key-update
//!    (v1.x — v0.6 has no loss recovery; loopback bus is reliable).
//! 4. RFC 9002 RTT estimator exactly — no "improvements" (v1.x).
//! 5. CUBIC default, NewReno fallback (DELIVERED, 1.7.0).
//! 6. L3's TLS wrapper drives the handshake; we frame the CRYPTO
//!    bytes + own the key-update side via
//!    `grpcx_crypto::quic_handshake::QuicEndpoint`.
//! 7. One UDP socket + N connections (v1.x); v0.6 runs over an
//!    in-memory bus.
//! 8. 0-RTT opt-in (v1.x).
//! 9. `#![no_std]` where feasible; `std` scoped to endpoint /
//!    socket modules.
//! 10. `quinn`-shaped public surface but narrower; runs on
//!     `grpcx-rt` (v1.x — v0.6's `pump` shape is runtime-agnostic).

extern crate alloc;
extern crate std;

#[doc(hidden)]
pub mod __fuzz_helpers;

mod connection;
mod frame;
mod initial;
mod packet;
mod protection;
mod sizes;
mod varint;

pub use connection::CongestionAlgorithm;
pub use connection::Connection as QuicConnection;
pub use frame::{EcnCounts, Frame, decode_frame, encode_frame, frame_varint_len};
pub use initial::{
    Side, build_initial_packet, build_initial_packet_with_initial_dcid, parse_initial_packet,
};
pub use packet::{
    LongHeader, LongHeaderType, PacketHeader, ShortHeader, decode_header, encode_header,
    varint_field_len,
};
pub use protection::{
    AeadSuite, apply_header_protection, protect_payload, remove_header_protection,
    unprotect_payload,
};

// ============================================================
// Connection identifiers + stream identifiers.
// ============================================================

/// A QUIC Connection ID — RFC 9000 §5.1, 0-20 bytes. We use the
/// max-length container; the active length is tracked alongside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    /// Raw bytes (zero-padded past `len`).
    pub bytes: [u8; 20],
    /// Active length, in bytes (0..=20).
    pub len: u8,
}

/// A QUIC Stream ID — RFC 9000 §2.1. Encoded as a varint on the
/// wire; the integer form is what callers see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(pub u64);

// ============================================================
// Errors.
// ============================================================

/// Failure mode reported by any L4 op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Malformed packet (header / length / version mismatch).
    BadPacket,
    /// Malformed frame.
    BadFrame,
    /// AEAD authentication failure or header-protection mismatch.
    BadCrypto,
    /// Peer closed the connection.
    ConnectionClosed,
    /// Stream closed (peer reset, finished, or stopped).
    StreamClosed,
    /// Idle / PTO timeout.
    Timeout,
    /// Endpoint bind / socket setup failed.
    BadEndpoint,
    /// Config validation failed.
    BadConfig,
    /// Implementation-specific failure.
    Internal,
}
