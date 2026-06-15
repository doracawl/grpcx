//! `Archive` / `Serialize` / `Deserialize` impls for the
//! built-in numeric primitives + `bool`. Wire format per
//! `../WIRE_FORMAT.md`: little-endian fixed-width with
//! `Archived<T> = T` on LE targets.

use crate::{Archive, Deserialize, Error, Serialize, Writer};

// ============================================================
// Numeric primitives — macro-generated.
//
// `Archived<T> = T` works because:
// - The wire format stores `T::to_le_bytes()`.
// - On LE targets that byte sequence IS the native `T` bit
//   pattern, so `*(ptr as *const T)` reads back the original
//   value with no byteswap. The crate's top-level
//   `#[cfg(target_endian = "big")] compile_error!` keeps that
//   invariant honest.
// - Alignment requirements match `core::mem::align_of::<T>()`,
//   which `validate` checks before transmuting.
// ============================================================

macro_rules! impl_numeric_primitive {
    ($t:ty) => {
        impl Archive for $t {
            type Archived = $t;

            fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
                let need = core::mem::size_of::<$t>();
                if bytes.len() < need {
                    return Err(Error::Truncated);
                }
                if bytes.len() != need {
                    // For a single primitive the buffer must be
                    // exactly the type's wire size. Composite
                    // types (v0.3+) take a different validation
                    // path that allows trailing bytes.
                    return Err(Error::BadLayout);
                }
                if (bytes.as_ptr() as usize) % core::mem::align_of::<$t>() != 0 {
                    return Err(Error::BadAlignment);
                }
                // SAFETY:
                // - length checked: bytes.len() == size_of::<T>()
                // - alignment checked: ptr is align_of::<T>()-
                //   aligned
                // - all bit patterns of $t (u*/i*/f*) are valid
                //   values; bool has a separate impl below
                Ok(unsafe { &*(bytes.as_ptr() as *const $t) })
            }
        }

        impl<W: Writer> Serialize<W> for $t {
            fn serialize(&self, writer: &mut W) -> Result<(), Error> {
                writer.write_all(&self.to_le_bytes())
            }
        }

        impl<D> Deserialize<D> for $t {
            fn deserialize(archived: &Self::Archived, _ctx: &mut D) -> Result<Self, Error> {
                Ok(*archived)
            }
        }
    };
}

impl_numeric_primitive!(u8);
impl_numeric_primitive!(u16);
impl_numeric_primitive!(u32);
impl_numeric_primitive!(u64);
impl_numeric_primitive!(u128);

impl_numeric_primitive!(i8);
impl_numeric_primitive!(i16);
impl_numeric_primitive!(i32);
impl_numeric_primitive!(i64);
impl_numeric_primitive!(i128);

impl_numeric_primitive!(f32);
impl_numeric_primitive!(f64);

// ============================================================
// bool — separate because the wire format has a value-level
// invariant (byte ∈ {0, 1}) the numeric primitives lack.
// ============================================================

impl Archive for bool {
    type Archived = bool;

    fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
        if bytes.is_empty() {
            return Err(Error::Truncated);
        }
        if bytes.len() != 1 {
            return Err(Error::BadLayout);
        }
        match bytes[0] {
            0 | 1 => {
                // SAFETY: `bool` has the same byte representation
                // as a `u8` ∈ {0, 1}. Alignment requirement of
                // `bool` is 1 (always met).
                Ok(unsafe { &*(bytes.as_ptr() as *const bool) })
            }
            _ => Err(Error::BadLayout),
        }
    }
}

impl<W: Writer> Serialize<W> for bool {
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        writer.write_all(&[u8::from(*self)])
    }
}

impl<D> Deserialize<D> for bool {
    fn deserialize(archived: &Self::Archived, _ctx: &mut D) -> Result<Self, Error> {
        Ok(*archived)
    }
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_bytes, to_bytes};

    #[test]
    fn u8_round_trip() {
        for v in [0u8, 1, 127, 128, 255] {
            let bytes = to_bytes(&v).unwrap();
            assert_eq!(bytes.len(), 1);
            assert_eq!(bytes[0], v);
            assert_eq!(*from_bytes::<u8>(&bytes).unwrap(), v);
        }
    }

    #[test]
    fn u32_le_wire_shape() {
        let bytes = to_bytes(&0xDEAD_BEEFu32).unwrap();
        assert_eq!(bytes, [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn i16_negative_round_trip() {
        let bytes = to_bytes(&-2i16).unwrap();
        assert_eq!(bytes, [0xFE, 0xFF]);
        assert_eq!(*from_bytes::<i16>(&bytes).unwrap(), -2);
    }

    #[test]
    fn f32_unity_round_trip() {
        let bytes = to_bytes(&1.0f32).unwrap();
        assert_eq!(bytes, [0x00, 0x00, 0x80, 0x3F]);
        assert_eq!(*from_bytes::<f32>(&bytes).unwrap(), 1.0);
    }

    #[test]
    fn bool_round_trip_both_values() {
        let t = to_bytes(&true).unwrap();
        let f = to_bytes(&false).unwrap();
        assert_eq!(t, [0x01]);
        assert_eq!(f, [0x00]);
        assert!(*from_bytes::<bool>(&t).unwrap());
        assert!(!*from_bytes::<bool>(&f).unwrap());
    }

    #[test]
    fn bool_other_byte_rejected() {
        for b in 2u8..=0xff {
            assert_eq!(from_bytes::<bool>(&[b]), Err(Error::BadLayout), "byte {b}");
        }
    }

    #[test]
    fn truncated_rejected() {
        assert_eq!(from_bytes::<u8>(&[]), Err(Error::Truncated));
        assert_eq!(from_bytes::<u16>(&[0x01]), Err(Error::Truncated));
        assert_eq!(from_bytes::<u64>(&[0; 7]), Err(Error::Truncated));
        assert_eq!(from_bytes::<bool>(&[]), Err(Error::Truncated));
    }

    #[test]
    fn oversize_rejected() {
        assert_eq!(from_bytes::<u8>(&[0, 0]), Err(Error::BadLayout));
        assert_eq!(from_bytes::<u32>(&[0; 5]), Err(Error::BadLayout));
    }
}
