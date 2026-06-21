//! Wire-format v2 frame ADTs — `FrameType`, `Flags`,
//! `Header`. Pure value types; codec lives in
//! `super::codec`.

use super::errors::WireError;

/// Fixed wire-format v2 header size in bytes (SPEC § 2).
pub const HEADER_LEN: usize = 16;

/// Frame type enum (SPEC § 4). Discriminant byte values
/// match the on-wire encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Open a new stream; carries call metadata.
    Begin = 0x01,
    /// Carry one message body on an open stream.
    Data = 0x02,
    /// Normal end-of-stream with trailing metadata.
    End = 0x03,
    /// Abnormal end-of-stream carrying a `Status`.
    Reset = 0x04,
    /// Connection-level liveness probe (`stream_id = 0`).
    Ping = 0x05,
    /// Connection-level liveness reply (`stream_id = 0`).
    Pong = 0x06,
}

impl FrameType {
    /// Convert an on-wire byte to a `FrameType`. Values
    /// outside `0x01..=0x06` map to `WireError::UnknownFrameType`.
    #[inline]
    pub fn from_u8(byte: u8) -> Result<Self, WireError> {
        match byte {
            0x01 => Ok(FrameType::Begin),
            0x02 => Ok(FrameType::Data),
            0x03 => Ok(FrameType::End),
            0x04 => Ok(FrameType::Reset),
            0x05 => Ok(FrameType::Ping),
            0x06 => Ok(FrameType::Pong),
            other => Err(WireError::UnknownFrameType(other)),
        }
    }

    /// On-wire byte for this frame type.
    #[inline]
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Flag bitset on the `flags` byte (SPEC § 2.2).
///
/// Bit 0 `END_STREAM`, bit 1 `HAS_METADATA`, bit 2
/// `CONTINUATION`. Bits 3..7 reserved and ignored on
/// receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(pub u8);

impl Flags {
    /// All bits cleared.
    pub const NONE: Self = Self(0);
    /// Bit 0 — this frame closes the stream from the
    /// sender's direction.
    pub const END_STREAM: Self = Self(0b0000_0001);
    /// Bit 1 — payload begins with a metadata block.
    pub const HAS_METADATA: Self = Self(0b0000_0010);
    /// Bit 2 — payload is the second-or-later fragment
    /// of a logical frame that exceeded one shmem slot.
    pub const CONTINUATION: Self = Self(0b0000_0100);

    /// True if every bit in `other` is set in `self`.
    #[inline]
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set every bit in `other` into a new `Flags`.
    #[inline]
    pub fn insert(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Raw bits.
    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

/// 16-byte fixed wire-format v2 frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Connection-scoped stream identifier (SPEC § 3).
    pub stream_id: u32,
    /// Frame type (SPEC § 4).
    pub frame_type: FrameType,
    /// Per-frame flag bits (SPEC § 2.2).
    pub flags: Flags,
    /// Number of metadata entries that follow at the
    /// start of the payload. Zero if `HAS_METADATA`
    /// flag is clear.
    pub metadata_count: u16,
    /// Length of the payload (metadata block +
    /// optional message bytes) in bytes (SPEC § 2.1).
    pub payload_len: u32,
}
