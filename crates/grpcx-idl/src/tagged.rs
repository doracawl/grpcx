//! Tagged unions — `Option<T>` and `Result<T, E>`.
//!
//! Wire format per `../WIRE_FORMAT.md` § Tagged unions:
//! - `[u32 LE discriminant]` at offset 0.
//! - Per-variant payload starts at `align_up(4, align_of::<variant>())`.
//! - Total wire size = `size_of::<Archived>()` (rustc's
//!   `#[repr(u32)]` enum layout, RFC 2195).
//! - Encoder writes every byte explicitly (per-variant padding
//!   plus trailing pad to total size). Decoder length-checks
//!   exactly + validates the inner payload sub-slice.
//!
//! User-defined enums wait for v0.6's derive macro; v0.5 hand-
//! rolls the two stdlib tagged unions only.

use crate::composite::align_up;
use crate::{Archive, Deserialize, Error, Serialize, Writer};
use core::mem::{align_of, size_of};

// ============================================================
// `ArchivedOption<T>`.
// ============================================================

/// `#[repr(u32)]` mirror of `Option<T>`. Same in-memory layout
/// as the wire format so the validate-then-transmute zero-copy
/// path is sound.
#[repr(u32)]
#[allow(missing_docs)] // variant docs duplicate the source enum
pub enum ArchivedOption<T>
where
    T: Archive,
    T::Archived: Sized,
{
    None = 0,
    Some(T::Archived) = 1,
}

impl<T> Archive for Option<T>
where
    T: Archive,
    T::Archived: Sized,
{
    type Archived = ArchivedOption<T>;

    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
        let need = size_of::<Self::Archived>();
        if bytes.len() < need {
            return Err(Error::Truncated);
        }
        if bytes.len() != need {
            return Err(Error::BadLayout);
        }
        if !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self::Archived>()) {
            return Err(Error::BadAlignment);
        }
        let disc = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        match disc {
            0 => {
                // None — nothing else to validate.
            }
            1 => {
                let payload_off = align_up(4, align_of::<T::Archived>());
                let payload_size = size_of::<T::Archived>();
                T::validate(&bytes[payload_off..payload_off + payload_size])?;
            }
            _ => return Err(Error::BadLayout),
        }
        // SAFETY:
        // - length + alignment checked above;
        // - discriminant in {0, 1};
        // - inner payload validated for the `Some` arm.
        Ok(unsafe { &*(bytes.as_ptr() as *const Self::Archived) })
    }
}

impl<W, T> Serialize<W> for Option<T>
where
    W: Writer,
    T: Serialize<W>,
    T::Archived: Sized,
{
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        let total = size_of::<<Self as Archive>::Archived>();
        match self {
            None => {
                writer.write_all(&0u32.to_le_bytes())?;
                pad_with_zeros(writer, 4, total)?;
            }
            Some(value) => {
                writer.write_all(&1u32.to_le_bytes())?;
                let payload_off = align_up(4, align_of::<T::Archived>());
                pad_with_zeros(writer, 4, payload_off)?;
                value.serialize(writer)?;
                let written = payload_off + size_of::<T::Archived>();
                pad_with_zeros(writer, written, total)?;
            }
        }
        Ok(())
    }
}

impl<D, T> Deserialize<D> for Option<T>
where
    T: Deserialize<D>,
    T::Archived: Sized,
{
    fn deserialize(archived: &Self::Archived, ctx: &mut D) -> Result<Self, Error> {
        match archived {
            ArchivedOption::None => Ok(None),
            ArchivedOption::Some(v) => Ok(Some(T::deserialize(v, ctx)?)),
        }
    }
}

// ============================================================
// `ArchivedResult<T, E>`.
// ============================================================

/// `#[repr(u32)]` mirror of `Result<T, E>`. Same layout
/// invariants as [`ArchivedOption`].
#[repr(u32)]
#[allow(missing_docs)]
pub enum ArchivedResult<T, E>
where
    T: Archive,
    E: Archive,
    T::Archived: Sized,
    E::Archived: Sized,
{
    Ok(T::Archived) = 0,
    Err(E::Archived) = 1,
}

impl<T, E> Archive for Result<T, E>
where
    T: Archive,
    E: Archive,
    T::Archived: Sized,
    E::Archived: Sized,
{
    type Archived = ArchivedResult<T, E>;

    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
        let need = size_of::<Self::Archived>();
        if bytes.len() < need {
            return Err(Error::Truncated);
        }
        if bytes.len() != need {
            return Err(Error::BadLayout);
        }
        if !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self::Archived>()) {
            return Err(Error::BadAlignment);
        }
        let disc = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        match disc {
            0 => {
                let payload_off = align_up(4, align_of::<T::Archived>());
                let payload_size = size_of::<T::Archived>();
                T::validate(&bytes[payload_off..payload_off + payload_size])?;
            }
            1 => {
                let payload_off = align_up(4, align_of::<E::Archived>());
                let payload_size = size_of::<E::Archived>();
                E::validate(&bytes[payload_off..payload_off + payload_size])?;
            }
            _ => return Err(Error::BadLayout),
        }
        // SAFETY: see ArchivedOption — same shape, two arms.
        Ok(unsafe { &*(bytes.as_ptr() as *const Self::Archived) })
    }
}

impl<W, T, E> Serialize<W> for Result<T, E>
where
    W: Writer,
    T: Serialize<W>,
    E: Serialize<W>,
    T::Archived: Sized,
    E::Archived: Sized,
{
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        let total = size_of::<<Self as Archive>::Archived>();
        match self {
            Ok(value) => {
                writer.write_all(&0u32.to_le_bytes())?;
                let payload_off = align_up(4, align_of::<T::Archived>());
                pad_with_zeros(writer, 4, payload_off)?;
                value.serialize(writer)?;
                let written = payload_off + size_of::<T::Archived>();
                pad_with_zeros(writer, written, total)?;
            }
            Err(value) => {
                writer.write_all(&1u32.to_le_bytes())?;
                let payload_off = align_up(4, align_of::<E::Archived>());
                pad_with_zeros(writer, 4, payload_off)?;
                value.serialize(writer)?;
                let written = payload_off + size_of::<E::Archived>();
                pad_with_zeros(writer, written, total)?;
            }
        }
        Ok(())
    }
}

impl<D, T, E> Deserialize<D> for Result<T, E>
where
    T: Deserialize<D>,
    E: Deserialize<D>,
    T::Archived: Sized,
    E::Archived: Sized,
{
    fn deserialize(archived: &Self::Archived, ctx: &mut D) -> Result<Self, Error> {
        match archived {
            ArchivedResult::Ok(v) => Ok(Ok(T::deserialize(v, ctx)?)),
            ArchivedResult::Err(v) => Ok(Err(E::deserialize(v, ctx)?)),
        }
    }
}

// ============================================================
// Helpers.
// ============================================================

/// Write `to - from` zero bytes to bring the writer from
/// position `from` (relative to this call's start) up to
/// position `to`. No-op if `to <= from`.
fn pad_with_zeros<W: Writer>(writer: &mut W, from: usize, to: usize) -> Result<(), Error> {
    let mut cur = from;
    while cur < to {
        writer.write_all(&[0u8])?;
        cur += 1;
    }
    Ok(())
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_u32_layout() {
        // ArchivedOption<u32>: disc u32 at 0, payload u32 at 4.
        // Total size = 8, align 4.
        assert_eq!(size_of::<ArchivedOption<u32>>(), 8);
        assert_eq!(align_of::<ArchivedOption<u32>>(), 4);
    }

    #[test]
    fn option_u64_layout() {
        // disc u32 at 0, pad to align-8, payload u64 at 8.
        // Total size = 16, align 8.
        assert_eq!(size_of::<ArchivedOption<u64>>(), 16);
        assert_eq!(align_of::<ArchivedOption<u64>>(), 8);
    }

    #[test]
    fn result_asymmetric_layout() {
        // Result<u8, u64>: align = max(4, 8) = 8.
        // Variant Ok: tag u32 + payload u8 at offset 4 = size 5,
        // padded to 16. Variant Err: tag u32 + pad to 8 + payload
        // u64 = size 16. Total enum size = 16, align 8.
        assert_eq!(size_of::<ArchivedResult<u8, u64>>(), 16);
        assert_eq!(align_of::<ArchivedResult<u8, u64>>(), 8);
    }
}
