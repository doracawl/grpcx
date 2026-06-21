//! Wire-format v2 codec error type.
//!
//! Every error variant maps to a specific clause of
//! `_dev/SPEC-wire-v2.md`. Receivers translate a
//! `WireError` into either a stream-level RESET or a
//! connection-level Internal RESET per SPEC § 7.

/// Failure surface for `decode_*` / `encode_*` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Slice shorter than the bytes required to decode
    /// the next item. `need` is the byte count this call
    /// would have read; `have` is the slice length.
    BufferTooShort {
        /// Bytes the decoder needed to complete the
        /// requested decode.
        need: usize,
        /// Bytes actually available in the input slice.
        have: usize,
    },

    /// `frame_type` byte outside `0x01..=0x06` (SPEC § 4).
    UnknownFrameType(u8),

    /// Metadata entry `name_len` is zero or exceeds
    /// `NAME_LEN_MAX` (SPEC § 5).
    InvalidMetadataNameLen(u16),

    /// Metadata block claims more bytes than the payload
    /// provides — either `name_len + value_len` overruns
    /// the remaining payload, or the cumulative block
    /// size exceeds `payload_len`.
    TruncatedMetadata,
}
