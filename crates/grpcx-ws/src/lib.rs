#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![no_std]

//! Layout: the public surface is the [`Frame`] / [`Opcode`]
//! / [`CloseCode`] / [`Message`] / [`MessageKind`] /
//! [`Handshake`] / [`WsError`] ADTs plus the entry-point
//! functions ([`encode_frame`] / [`decode_frame`] /
//! [`perform_client_handshake_request`] /
//! [`parse_server_handshake_response`] /
//! [`parse_client_handshake_request`] /
//! [`build_server_handshake_response`]). v0.1 ships the
//! **shape** of every public item with `unimplemented!()`
//! bodies; each v0.x checkpoint replaces a slice with real
//! code.
//!
//! Design intent (per `../../REFACTOR-v9-v0.1-bootstrap.md`):
//!
//! 1. **Spec-faithful.** RFC 6455 § 5.2 frame format, § 5.3
//!    masking, § 4 handshake, § 7 close codes. Where the
//!    spec allows MAY behaviour, document the chosen
//!    interop position.
//! 2. **Pure framing.** No syscalls / IO / async / network.
//!    Caller drives `decode_frame(bytes)` /
//!    `encode_frame(frame, out)` against any byte buffer.
//! 3. **Dual surface.** [`Frame`] exposes individual header
//!    bits; [`Message`] reassembles fragmented frames into
//!    a single payload. Each altitude has its own use case.
//! 4. **Symmetric encode/decode.** Every wire form a
//!    decoder accepts, the encoder produces, and vice
//!    versa.
//! 5. **Reject ambiguity.** Reserved bits set, unknown
//!    opcodes, mismatched masking, oversize payload — all
//!    [`WsError`]. Don't silently drop bits.
//! 6. `no_std + alloc`. Works in firmware contexts.
//! 7. Stones-only deps: `base64-rfc4648` + `sha1-rfc3174`
//!    (both 0-runtime-dep pure-Rust `no_std + alloc`
//!    crates from the in-house stones workspace at
//!    ../../../stones). No external runtime deps.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

mod close;
mod codec;
mod extensions;
mod handshake;
mod reassembler;
mod sizes;

pub use close::{decode_close_payload, encode_close_payload};
pub use extensions::{
    ExtensionOffer, PermessageDeflateOffer, find_offer_by_name, format_extension_header,
    parse_extension_header, parse_extension_offer,
};
pub use handshake::{derive_accept_key, verify_accept_key};
pub use reassembler::{Reassembler, ReassemblyOutput};

// ============================================================
// Frame model.
// ============================================================

/// A single WebSocket frame as defined in RFC 6455 § 5.2.
///
/// `Frame` exposes individual header bits so callers
/// building fragmented messages or sending custom-extension
/// control frames have direct control. For the
/// payload-level abstraction (post-reassembly), see
/// [`Message`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frame {
    /// `FIN` bit — 1 = last frame of a message; 0 = a
    /// continuation frame follows.
    pub fin: bool,
    /// Reserved bit 1 — must be 0 unless an extension
    /// negotiated it.
    pub rsv1: bool,
    /// Reserved bit 2 — must be 0 unless an extension
    /// negotiated it.
    pub rsv2: bool,
    /// Reserved bit 3 — must be 0 unless an extension
    /// negotiated it.
    pub rsv3: bool,
    /// 4-bit opcode.
    pub opcode: Opcode,
    /// 4-byte XOR masking key if the frame is masked
    /// (client→server frames MUST be; server→client
    /// frames MUST NOT be).
    pub mask: Option<[u8; 4]>,
    /// Frame payload bytes — already unmasked on decode.
    pub payload: Vec<u8>,
}

/// WebSocket frame opcode (RFC 6455 § 5.2 byte 0 bits 0-3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    /// `0x0` — Continuation. Carries the next chunk of a
    /// fragmented data message.
    #[default]
    Continuation,
    /// `0x1` — Text. Payload is UTF-8.
    Text,
    /// `0x2` — Binary. Payload is opaque bytes.
    Binary,
    /// `0x8` — Close (control frame).
    Close,
    /// `0x9` — Ping (control frame).
    Ping,
    /// `0xA` — Pong (control frame).
    Pong,
}

impl Opcode {
    /// `true` for control opcodes (`Close` / `Ping` /
    /// `Pong`). Control frames MUST have `FIN=1` and
    /// payload ≤ 125 bytes per RFC 6455 § 5.5.
    pub fn is_control(&self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

// ============================================================
// Close codes.
// ============================================================

/// Connection close code (RFC 6455 § 7.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCode {
    /// 1000 — Normal closure.
    Normal,
    /// 1001 — Endpoint going away (e.g. server shutdown).
    GoingAway,
    /// 1002 — Protocol error.
    ProtocolError,
    /// 1003 — Endpoint cannot accept this data type.
    UnsupportedData,
    /// 1007 — Payload does not match the type implied by
    /// the opcode (e.g. non-UTF-8 in a Text message).
    InvalidPayload,
    /// 1008 — Generic policy violation.
    PolicyViolation,
    /// 1009 — Message too large to process.
    MessageTooLarge,
    /// 1010 — Client required extension(s) the server did
    /// not honour.
    ExtensionRequired,
    /// 1011 — Internal server error.
    InternalError,
    /// IANA-registered (3000-3999) or application-private
    /// (4000-4999) codes. Reserved codes (1004, 1005,
    /// 1006, 1015) are rejected by `from_u16`.
    Custom(u16),
}

// ============================================================
// Message (post-reassembly).
// ============================================================

/// A reassembled WebSocket message — the concatenation of
/// one or more [`Frame`]s with the same data opcode.
///
/// Populated by v0.4's reassembly logic. Until then, only
/// the type shape exists.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    /// Whether the payload is UTF-8 text or opaque bytes.
    pub kind: MessageKind,
    /// Reassembled payload.
    pub payload: Vec<u8>,
}

/// Distinguishes [`MessageKind::Text`] (UTF-8 payload) from
/// [`MessageKind::Binary`] (opaque bytes).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Text message — payload is UTF-8.
    #[default]
    Text,
    /// Binary message — payload is opaque bytes.
    Binary,
}

// ============================================================
// Handshake.
// ============================================================

/// HTTP/1.1 upgrade-handshake state — carries both sides
/// of the request/response exchange.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// Client `Sec-WebSocket-Key` header value (the 16-byte
    /// base64-encoded nonce the client picks).
    pub request_key: String,
    /// Server `Sec-WebSocket-Accept` header value —
    /// SHA-1(request_key + WS_MAGIC_GUID), base64-encoded.
    pub accept_key: String,
    /// Subprotocols offered (client side) or selected
    /// (server side).
    pub subprotocols: Vec<String>,
    /// Extensions offered (client side) or selected
    /// (server side); `permessage-deflate` lives here at
    /// v0.6.
    pub extensions: Vec<String>,
}

// ============================================================
// Errors.
// ============================================================

/// Failure modes from the frame codec and the handshake
/// codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsError {
    /// Input ended mid-decode.
    Truncated,
    /// Opcode value not in {0x0, 0x1, 0x2, 0x8, 0x9, 0xA}.
    InvalidOpcode(u8),
    /// RSV1 / RSV2 / RSV3 set without an extension
    /// negotiating them.
    ReservedBitSet,
    /// Text message payload was not valid UTF-8.
    InvalidUtf8,
    /// Close code is reserved (1004 / 1005 / 1006 / 1015)
    /// or otherwise unsupported.
    InvalidCloseCode(u16),
    /// Payload exceeds the implementation's accept ceiling
    /// or the length field's available range (extended-
    /// length bit issue).
    PayloadTooLarge,
    /// Client→server frame missing its mask, or server→
    /// client frame carrying one.
    MaskingMismatch,
    /// Control frame (`Close` / `Ping` / `Pong`) with
    /// `FIN=0` — RFC 6455 § 5.5 forbids fragmentation
    /// of control frames.
    FragmentedControlFrame,
    /// Generic protocol violation surfaced by the v0.4
    /// reassembler: continuation frame at empty state, or
    /// new data frame while a previous fragmented message
    /// is still in progress. The static string captures
    /// which sub-case.
    ProtocolError(&'static str),
    /// HTTP/1.1 upgrade handshake malformed.
    BadHandshake(&'static str),
}

// ============================================================
// Constants.
// ============================================================

/// RFC 6455 § 4.2.2 step 5 magic GUID — concatenated with
/// the client `Sec-WebSocket-Key` and SHA-1'd to derive the
/// server `Sec-WebSocket-Accept`.
pub const WS_MAGIC_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `Sec-WebSocket-Version` value the crate accepts (the
/// only spec-current version).
pub const WS_VERSION: u8 = 13;

/// `Upgrade:` header value (case-insensitive on the wire).
pub const WS_UPGRADE: &str = "websocket";

/// Control-frame payload ceiling — RFC 6455 § 5.5.
pub const WS_MAX_CONTROL_PAYLOAD: usize = 125;

// ============================================================
// Entry points.
// ============================================================

/// Encode a [`Frame`] onto a byte buffer per RFC 6455
/// § 5.2.
///
/// v0.1 is API-only; bodies fill in across v0.2-v0.7.
pub fn encode_frame(frame: &Frame, out: &mut Vec<u8>) {
    codec::encode(frame, out)
}

/// Decode the leading frame from a byte buffer per
/// RFC 6455 § 5.2.
///
/// Returns `(decoded_frame, bytes_consumed)`. The returned
/// frame has its payload already unmasked (if the wire
/// carried a mask).
///
/// # Errors
/// - [`WsError::Truncated`] — input shorter than the
///   frame's header + payload demand.
/// - [`WsError::InvalidOpcode`] — opcode outside the
///   spec-defined set.
/// - [`WsError::ReservedBitSet`] — RSV1/2/3 set without
///   extension negotiation.
/// - [`WsError::PayloadTooLarge`] — length encoding
///   violation (overlong form, or 64-bit length with MSB
///   set).
/// - [`WsError::MaskingMismatch`] — caller-context
///   directionality rule violated (enforced once the
///   v0.2 codec lands; v0.1 reports nothing).
pub fn decode_frame(bytes: &[u8]) -> Result<(Frame, usize), WsError> {
    codec::decode(bytes)
}

/// Build the client `GET …` upgrade request (RFC 6455
/// § 4.2.1) as a single HTTP/1.1 message string.
///
/// `host` and `path` are written into the request line and
/// `Host:` header. The caller supplies the random
/// `Sec-WebSocket-Key` (16-byte base64-encoded nonce) via
/// `handshake.request_key`; v0.5 doesn't generate
/// randomness internally. `handshake.subprotocols` and
/// `handshake.extensions` populate the optional headers.
/// `handshake.accept_key` is ignored on the client side.
///
/// # Note
/// v0.1's signature was `(host, path) -> String`; v0.5 adds
/// the `handshake` parameter so the caller can supply the
/// nonce + subprotocols + extensions. Pre-1.0 break locked
/// at v0.5.
pub fn perform_client_handshake_request(host: &str, path: &str, handshake: &Handshake) -> String {
    handshake::perform_client_handshake_request(host, path, handshake)
}

/// Parse the server `HTTP/1.1 101 Switching Protocols`
/// response per RFC 6455 § 4.2.2 and return the negotiated
/// [`Handshake`] state.
pub fn parse_server_handshake_response(bytes: &[u8]) -> Result<Handshake, WsError> {
    handshake::parse_server_handshake_response(bytes)
}

/// Parse the client `GET …` upgrade request per RFC 6455
/// § 4.2.1 and return the negotiated [`Handshake`] state.
pub fn parse_client_handshake_request(bytes: &[u8]) -> Result<Handshake, WsError> {
    handshake::parse_client_handshake_request(bytes)
}

/// Build the server `HTTP/1.1 101 Switching Protocols`
/// response (RFC 6455 § 4.2.2) as a single HTTP/1.1
/// message string.
pub fn build_server_handshake_response(handshake: &Handshake) -> String {
    handshake::build_server_handshake_response(handshake)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn error_is_compact() {
        // `WsError` carries at most `(u8)` / `(u16)` /
        // `(&'static str)`. On 64-bit, the fat-pointer arm
        // is 16 + 8 = 24 bytes. The 32-byte bound matches
        // the L7 / L8 `Error` bound for consistency.
        assert!(size_of::<WsError>() <= 32);
    }

    #[test]
    fn frame_default_smoke() {
        // Smoke: `Frame::default()` constructs cleanly and
        // matches a hand-written default (fin = false,
        // opcode = Continuation, no mask, empty payload).
        let f = Frame::default();
        assert!(!f.fin);
        assert!(!f.rsv1);
        assert!(!f.rsv2);
        assert!(!f.rsv3);
        assert_eq!(f.opcode, Opcode::Continuation);
        assert!(f.mask.is_none());
        assert!(f.payload.is_empty());
    }
}
