//! Variable-length types — `Vec<u8>` + `String`.
//!
//! Wire format per `../WIRE_FORMAT.md` § Variable-length:
//! `[u32 LE length][N bytes payload]`. Buffer alignment = 4
//! (the length prefix's). UTF-8 validation for `String` is
//! lazy — `from_bytes::<String>` only checks structural
//! invariants (length + alignment); the accessor
//! [`ArchivedString::as_str`] runs the UTF-8 check on demand.
//!
//! Out of scope for v0.4 (deferred to v0.5+):
//! - Typed payload `Vec<T>` / `&[T]` for `T ≠ u8` — alignment
//!   padding when `align_of::<T::Archived>() > 4` needs
//!   per-element offset math or relative-offset segmentation.
//! - Standalone `&[u8]` / `&str` Archive impls — the trait's
//!   `type Archived` can't carry a lifetime; callers reach
//!   for the same zero-copy story via
//!   `from_bytes::<Vec<u8>>(buf) -> &ArchivedBytes` +
//!   `.as_slice()`.

use crate::{Archive, Deserialize, Error, Serialize, Writer};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ============================================================
// ArchivedBytes — the shared on-wire byte-payload view.
// ============================================================

/// Archived view of a byte payload — `[u32 LE length][N bytes]`.
///
/// `#[repr(transparent)]` over `[u8]` so [`Archive::validate`]
/// can transmute `&[u8] → &ArchivedBytes` with zero cost. The
/// length sits at the first 4 bytes; accessors read it on
/// demand.
#[repr(transparent)]
pub struct ArchivedBytes([u8]);

impl ArchivedBytes {
    /// Payload length (the `u32 LE` prefix decoded as `usize`).
    pub fn len(&self) -> usize {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]) as usize
    }

    /// `true` if the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow the payload bytes (the `N` bytes following the
    /// length prefix). Zero-copy: the slice points directly
    /// into the original buffer.
    pub fn as_slice(&self) -> &[u8] {
        let len = self.len();
        &self.0[4..4 + len]
    }
}

// ============================================================
// ArchivedString — UTF-8 wrapper over ArchivedBytes.
// ============================================================

/// Archived view of a UTF-8 string. Same wire shape as
/// [`ArchivedBytes`] but exposes `as_str` for the UTF-8 check.
///
/// `#[repr(transparent)]` over [`ArchivedBytes`] so the wire
/// format is identical.
#[repr(transparent)]
pub struct ArchivedString(ArchivedBytes);

impl ArchivedString {
    /// Byte length of the underlying payload.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` if the underlying payload is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrow the underlying bytes without the UTF-8 check.
    /// Useful for callers that already trust the producer.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Validate UTF-8 and return `&str`. **Lazy**:
    /// [`Archive::validate`] does not run this — invalid UTF-8
    /// will surface here.
    pub fn as_str(&self) -> Result<&str, Error> {
        core::str::from_utf8(self.as_bytes()).map_err(|_| Error::Utf8)
    }
}

// ============================================================
// Internal: validate the length-prefix header + return a
// transmuted `&ArchivedBytes`. Used by both Vec<u8> and
// String impls.
// ============================================================

fn validate_bytes(bytes: &[u8]) -> Result<&ArchivedBytes, Error> {
    if bytes.len() < 4 {
        return Err(Error::Truncated);
    }
    if !(bytes.as_ptr() as usize).is_multiple_of(4) {
        return Err(Error::BadAlignment);
    }
    let declared = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let need = 4usize.checked_add(declared).ok_or(Error::BadLayout)?;
    if bytes.len() < need {
        return Err(Error::Truncated);
    }
    if bytes.len() != need {
        return Err(Error::BadLayout);
    }
    // SAFETY:
    // - `ArchivedBytes` is `#[repr(transparent)]` over `[u8]`;
    //   the DST coercion produces a fat pointer with the same
    //   data pointer + length as the input slice.
    // - Length + alignment checked above.
    let archived: &ArchivedBytes = unsafe { &*(bytes as *const [u8] as *const ArchivedBytes) };
    Ok(archived)
}

// ============================================================
// Vec<u8> impls.
// ============================================================

impl Archive for Vec<u8> {
    type Archived = ArchivedBytes;

    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
        validate_bytes(bytes)
    }
}

impl<W: Writer> Serialize<W> for Vec<u8> {
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        let len: u32 = self.len().try_into().map_err(|_| Error::BadLayout)?;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(self)?;
        Ok(())
    }
}

impl<D> Deserialize<D> for Vec<u8> {
    fn deserialize(archived: &Self::Archived, _ctx: &mut D) -> Result<Self, Error> {
        Ok(archived.as_slice().to_vec())
    }
}

// ============================================================
// String impls.
// ============================================================

impl Archive for String {
    type Archived = ArchivedString;

    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
        let archived_bytes = validate_bytes(bytes)?;
        // SAFETY: `ArchivedString` is `#[repr(transparent)]`
        // over `ArchivedBytes`, so the layouts are identical.
        let archived_string: &ArchivedString =
            unsafe { &*(archived_bytes as *const ArchivedBytes as *const ArchivedString) };
        Ok(archived_string)
    }
}

impl<W: Writer> Serialize<W> for String {
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        let bytes = self.as_bytes();
        let len: u32 = bytes.len().try_into().map_err(|_| Error::BadLayout)?;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(bytes)?;
        Ok(())
    }
}

impl<D> Deserialize<D> for String {
    fn deserialize(archived: &Self::Archived, _ctx: &mut D) -> Result<Self, Error> {
        // Materialise an owned `String`. UTF-8 is checked here
        // (Deserialize is the eager path; the zero-copy
        // `as_str` accessor is the lazy path).
        let s = archived.as_str()?;
        Ok(s.to_string())
    }
}

// ============================================================
// Unit tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_bytes_layout_transparent_over_slice() {
        // Sanity: `ArchivedBytes` is layout-equivalent to
        // `[u8]`, so a length-prefixed buffer transmutes cleanly.
        // Use `to_bytes` to get a heap-allocated buffer — Vec<u8>
        // from malloc is at least 4-aligned in practice on every
        // target the crate supports.
        let v: Vec<u8> = alloc::vec![0xAB, 0xCD];
        let bytes = crate::to_bytes(&v).unwrap();
        let archived = <Vec<u8> as Archive>::validate(&bytes).unwrap();
        assert_eq!(archived.len(), 2);
        assert_eq!(archived.as_slice(), &[0xAB, 0xCD]);
    }
}
