#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Layout: the working public surface is [`Connection`] — the H/3
//! session that wraps a [`grpcx_quic::QuicConnection`] and drives
//! HTTP/3 framing + QPACK over its bidirectional + unidirectional
//! streams. v0.5 ships the loopback path end-to-end: SETTINGS
//! exchange, GET-request round-trip, GOAWAY.
//!
//! Design intent (per
//! `../../REFACTOR-v5-v0.1-bootstrap.md`):
//!
//! 1. Zero-alloc on the request hot path (caller-provided
//!    buffers; v1.x adds borrow-only `FrameRef`).
//! 2. QPACK static-only first (v0.3); dynamic table layers on
//!    top in v0.6.
//! 3. One control stream per direction (RFC 9114 §6.2.1);
//!    duplicates → `H3_STREAM_CREATION_ERROR`.
//! 4. Unknown frame types on request streams are reserved-skip,
//!    not error (RFC 9114 §7.2.8).
//! 5. Unknown stream types on uni streams are reserved-skip
//!    (abort only on duplicate typed control / QPACK streams).
//! 6. No HPACK fallback — HTTP/2 is L9's problem.
//! 7. No HTTP-semantics validation beyond pseudo-header slot
//!    checks; body / trailer / content-length rules are
//!    application-layer.
//! 8. The pump-shaped sync API mirrors L4 v1.x; async-fn over
//!    `grpcx-rt` lands when the higher layers demand it.
//! 9. `h3`-shaped public surface but narrower; [`RequestStream`]
//!    is an opaque handle, all I/O goes through [`Connection`]
//!    methods (same collapse L4 did between v0.1's
//!    `SendStream`/`RecvStream` and v0.6's `QuicConnection`).
//! 10. GOAWAY carries the highest **accepted** stream ID
//!     (RFC 9114 §5.2). Off-by-one bugs here are pinned in
//!     v0.5's acceptance test.

extern crate alloc;
extern crate std;

#[doc(hidden)]
pub mod __fuzz_helpers;

mod connection;
mod frame;
mod qpack;
mod qpack_dynamic;
mod sizes;
mod stream;
mod varint;

use alloc::vec::Vec;

pub use connection::{Connection, RequestStream};
pub use frame::{Frame, PriorityKind, SettingPair, decode_frame, encode_frame, frame_varint_len};
pub use qpack::{
    decode_field_section, decode_field_section_with_table, encode_field_section, find_static,
    find_static_name, static_entry,
};
pub use qpack_dynamic::{
    DecoderInstruction, DynamicEntry, DynamicTable, EncoderInstruction, decode_decoder_instruction,
    decode_encoder_instruction, encode_decoder_instruction, encode_encoder_instruction,
};
pub use stream::{
    StreamType, is_critical, is_unique_per_direction, read_stream_type, write_stream_type,
};

// ============================================================
// Header — one HTTP/3 (pseudo-)header pair.
// ============================================================

/// One HTTP/3 header pair. Names are typically lower-case ASCII
/// (RFC 9114 §4.2); pseudo-headers start with `:`
/// (`:method` / `:scheme` / `:authority` / `:path` for requests;
/// `:status` for responses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Header name (lower-case ASCII for ordinary headers;
    /// leading `:` for pseudo-headers).
    pub name: Vec<u8>,
    /// Header value (opaque bytes; UTF-8 in practice for most
    /// fields).
    pub value: Vec<u8>,
}

// ============================================================
// Settings — RFC 9114 §7.2.4 SETTINGS payload.
// ============================================================

/// SETTINGS payload (RFC 9114 §7.2.4 + RFC 9204 §5). Sent once
/// per direction on the control stream as the first frame. Each
/// field is `Option<u64>` because RFC 9114 lets peers omit any
/// setting (the omitted value falls back to the protocol default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `SETTINGS_MAX_FIELD_SECTION_SIZE` (RFC 9114 §4.2.2) — soft
    /// upper bound on the total uncompressed size of a request /
    /// response header section. Default: unlimited.
    pub max_field_section_size: Option<u64>,
    /// `SETTINGS_QPACK_MAX_TABLE_CAPACITY` (RFC 9204 §5) —
    /// maximum dynamic-table capacity in bytes. Default: 0
    /// (disables the dynamic table).
    pub qpack_max_table_capacity: Option<u64>,
    /// `SETTINGS_QPACK_BLOCKED_STREAMS` (RFC 9204 §5) — maximum
    /// number of streams that may be blocked awaiting a QPACK
    /// header-block decode. Default: 0.
    pub qpack_blocked_streams: Option<u64>,
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL` (RFC 9220) — `true`
    /// advertises that the server accepts the extended CONNECT
    /// method used by WebTransport.
    pub enable_connect_protocol: bool,
}

// ============================================================
// StatusCode — H/3 stream + connection error codes (RFC 9114 §8).
// ============================================================

/// H/3 error code carried by `RESET_STREAM` / `STOP_SENDING` /
/// `CONNECTION_CLOSE`. RFC 9114 §8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// `H3_NO_ERROR` (0x0100).
    NoError,
    /// `H3_GENERAL_PROTOCOL_ERROR` (0x0101).
    GeneralProtocolError,
    /// `H3_INTERNAL_ERROR` (0x0102).
    InternalError,
    /// `H3_STREAM_CREATION_ERROR` (0x0103).
    StreamCreationError,
    /// `H3_CLOSED_CRITICAL_STREAM` (0x0104).
    ClosedCriticalStream,
    /// `H3_FRAME_UNEXPECTED` (0x0105).
    FrameUnexpected,
    /// `H3_FRAME_ERROR` (0x0106).
    FrameError,
    /// `H3_EXCESSIVE_LOAD` (0x0107).
    ExcessiveLoad,
    /// `H3_ID_ERROR` (0x0108).
    IdError,
    /// `H3_SETTINGS_ERROR` (0x0109).
    SettingsError,
    /// `H3_MISSING_SETTINGS` (0x010a).
    MissingSettings,
    /// `H3_REQUEST_REJECTED` (0x010b).
    RequestRejected,
    /// `H3_REQUEST_CANCELLED` (0x010c).
    RequestCancelled,
    /// `H3_REQUEST_INCOMPLETE` (0x010d).
    RequestIncomplete,
    /// `H3_MESSAGE_ERROR` (0x010e).
    MessageError,
    /// `H3_CONNECT_ERROR` (0x010f).
    ConnectError,
    /// `H3_VERSION_FALLBACK` (0x0110).
    VersionFallback,
}

// ============================================================
// Errors.
// ============================================================

/// Failure mode reported by any L5 op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Malformed H/3 frame on the wire.
    BadFrame,
    /// QPACK decoder error (RFC 9204 §6 — `QPACK_DECOMPRESSION_FAILED`,
    /// `QPACK_ENCODER_STREAM_ERROR`, `QPACK_DECODER_STREAM_ERROR`).
    QpackDecode,
    /// QPACK encoder error.
    QpackEncode,
    /// Underlying QUIC error from L4.
    Quic(grpcx_quic::Error),
    /// Implementation-specific failure.
    Internal,
}

impl From<grpcx_quic::Error> for Error {
    fn from(e: grpcx_quic::Error) -> Self {
        Error::Quic(e)
    }
}
