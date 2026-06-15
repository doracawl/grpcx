#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![no_std]

//! Layout: the public surface is the three core traits
//! ([`Archive`] / [`Serialize`] / [`Deserialize`]) plus the
//! free-function entry points [`to_bytes`] and [`from_bytes`].
//! v0.1 ships the **shape** of every public item with
//! `unimplemented!()` bodies; each v0.x checkpoint replaces a
//! slice with real code.
//!
//! Design intent (per `../../REFACTOR-v7-v0.1-bootstrap.md`):
//!
//! 1. Zero-copy on the hot path — `from_bytes::<T>(&[u8])`
//!    returns `&Archived<T>` with no allocation.
//! 2. Little-endian everywhere; one less byteswap on any
//!    relevant CPU.
//! 3. Schema-driven layout — wire format does NOT carry
//!    field tags; the compile-time-known schema does
//!    (rkyv / Cap'n Proto trade-off, not protobuf's).
//! 4. `no_std + alloc` — works in firmware contexts. No
//!    `std` leakage in public API.
//! 5. No external runtime deps.
//! 6. Validation is a separate step the caller opts into —
//!    the hot path skips it. `from_bytes` validates
//!    structural invariants (offset bounds, alignment); type-
//!    level invariants (UTF-8, enum discriminants) defer to
//!    accessor APIs on `Archived<T>`.
//! 7. `#[derive(GrpcxIdl)]` proc-macro (lands v0.6) generates
//!    all three trait impls for user types.
//! 8. `Error` is a small enum (no formatter); callers map
//!    into their own error types.

extern crate alloc;

#[doc(hidden)]
pub mod __fuzz_helpers;

mod composite;
mod primitives;
mod sizes;
mod tagged;
mod varlen;

pub use composite::{
    ArchivedTuple2, ArchivedTuple3, ArchivedTuple4, ArchivedTuple5, ArchivedTuple6, ArchivedTuple7,
    ArchivedTuple8,
};
pub use tagged::{ArchivedOption, ArchivedResult};
pub use varlen::{ArchivedBytes, ArchivedString};

use alloc::vec::Vec;

// Little-endian-only ground rule for the wire format (v0.2 →
// onward). See `WIRE_FORMAT.md` § Endianness. v1.x revisits if
// a real BE host project appears.
#[cfg(target_endian = "big")]
compile_error!(
    "grpcx-idl currently requires little-endian targets; \
     big-endian support lands in v1.x via byteswap accessor \
     wrappers."
);

// ============================================================
// Core traits.
// ============================================================

/// A type that has a defined zero-copy in-place layout.
///
/// The associated type [`Archive::Archived`] is the byte-view
/// type that [`from_bytes`] returns by reference. Implementors
/// guarantee that any `&[u8]` returned by a successful
/// [`Serialize`] can be cast to `&Archived` with no allocation
/// or parsing — only validation.
///
/// For most user types the [`#[derive(GrpcxIdl)]`] proc-macro
/// (v0.6) generates this impl. Hand-written impls are the
/// escape hatch for unusual layout requirements.
///
/// [`#[derive(GrpcxIdl)]`]: <https://docs.rs/grpcx-idl>
pub trait Archive {
    /// The zero-copy byte-view type returned by [`from_bytes`].
    /// Typically `#[repr(C)]` and pointer-stable in memory.
    /// `?Sized` so variable-length types (v0.4+) can use a
    /// `#[repr(transparent)]` DST over `[u8]`.
    type Archived: ?Sized;

    /// Validate `bytes` is a well-formed archived `Self` and
    /// return a reference into the slice as `&Archived`.
    /// **Zero-copy**: no allocation, no parsing — only
    /// structural validation (length, alignment, type-level
    /// invariants).
    ///
    /// Implementations must:
    /// 1. Reject inputs that don't fit the wire size for `Self`
    ///    with [`Error::Truncated`] (or [`Error::BadLayout`] if
    ///    the buffer is longer than the type fixed wire size
    ///    and the impl doesn't allow trailing data).
    /// 2. Reject misaligned buffer starts with
    ///    [`Error::BadAlignment`].
    /// 3. Reject value-level invariants (e.g. `bool` byte not
    ///    in `{0, 1}`) with [`Error::BadLayout`].
    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error>;
}

/// A type that can be encoded into a [`Writer`].
///
/// `Serialize` is parameterised over the writer trait so the
/// caller chooses the byte sink (default: a `Vec<u8>` via the
/// blanket [`Writer for Vec<u8>`] impl).
///
/// [`Writer for Vec<u8>`]: Writer
pub trait Serialize<W: Writer>: Archive {
    /// Write `self` into `writer`. The wire layout must match
    /// what [`Archive::Archived`] expects so the byte sink
    /// produced by this call round-trips through
    /// [`from_bytes`].
    fn serialize(&self, writer: &mut W) -> Result<(), Error>;
}

/// A type that can be materialised back from an archived view
/// plus a deserialization context `D`. The context carries
/// any extra borrow lifetimes or allocators the implementor
/// needs.
///
/// Zero-copy callers typically operate directly on
/// `&Archived<T>` from [`from_bytes`] and only reach for
/// `Deserialize` when they need an owned `T` (e.g. to move
/// across threads).
pub trait Deserialize<D>: Archive {
    /// Build an owned `Self` from the archived view + the
    /// deserialization context.
    fn deserialize(archived: &Self::Archived, ctx: &mut D) -> Result<Self, Error>
    where
        Self: Sized;
}

/// Minimal byte-sink trait. The crate stays
/// `no_std + alloc`-friendly by depending only on `write_all`
/// and `position`; the standard-library `std::io::Write`
/// blanket impl is the caller's choice to add.
///
/// Implemented out of the box for [`Vec<u8>`] (the typical
/// budget-free buffer) — call [`to_bytes`] for that path.
pub trait Writer {
    /// Append `bytes` to the sink.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error>;
    /// Current byte offset from the start of the sink.
    /// Used for offset-based references in the wire format.
    fn position(&self) -> usize;
}

impl Writer for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.extend_from_slice(bytes);
        Ok(())
    }
    fn position(&self) -> usize {
        self.len()
    }
}

// ============================================================
// Free-function entry points.
// ============================================================

/// Drive serialization end-to-end with a default writer
/// (`Vec<u8>`). Convenience wrapper around
/// [`Serialize::serialize`] for callers who don't need a
/// custom writer.
pub fn to_bytes<T>(value: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize<Vec<u8>>,
{
    let mut buf = Vec::new();
    value.serialize(&mut buf)?;
    Ok(buf)
}

/// Validate `bytes` is a well-formed archived `T` and return a
/// reference into the slice as `&Archived<T>`. **Zero-copy**:
/// no allocation, no parsing — only structural validation
/// (length, alignment, type-level invariants).
///
/// Thin wrapper around [`Archive::validate`] kept for
/// caller-side ergonomics (mirrors the [`to_bytes`] shape).
pub fn from_bytes<T>(bytes: &[u8]) -> Result<&<T as Archive>::Archived, Error>
where
    T: Archive,
{
    T::validate(bytes)
}

// ============================================================
// Errors.
// ============================================================

/// Failure mode reported by any L7 op. Small (≤ 32 bytes on
/// aarch64-darwin) — callers map into their own error types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Input slice ended mid-value (e.g. a length field
    /// promises N bytes but only M < N are present).
    Truncated,
    /// Wire-layout invariant violation: an offset points
    /// outside the buffer, a length field exceeds its bound,
    /// or a discriminant byte is out of range for the
    /// declared enum.
    BadLayout,
    /// Pointer alignment requirement not met. Returned by
    /// [`from_bytes`] when the buffer's start address isn't
    /// aligned to `align_of::<Archived<T>>()`.
    BadAlignment,
    /// UTF-8 validation failed on an archived `str` accessor.
    /// Only raised by accessor APIs, never by [`from_bytes`]
    /// itself.
    Utf8,
    /// Caller-defined failure carrying a static description.
    /// Used by implementors of [`Serialize`] / [`Deserialize`]
    /// for protocol-specific reasons.
    Custom(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_compact() {
        // Smoke: the Error enum stays small enough to live in
        // a Result without bloating the hot path. The exact
        // upper bound is platform-dependent; this guards
        // against accidental nested-Vec growth.
        assert!(core::mem::size_of::<Error>() <= 32);
    }

    #[test]
    fn writer_for_vec_tracks_position() {
        let mut buf: Vec<u8> = Vec::new();
        assert_eq!(buf.position(), 0);
        buf.write_all(b"abc").unwrap();
        assert_eq!(buf.position(), 3);
        buf.write_all(b"de").unwrap();
        assert_eq!(buf.position(), 5);
    }
}
