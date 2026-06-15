//! Fixed-size composite types — tuples + fixed arrays.
//!
//! Wire format per `../WIRE_FORMAT.md` § Padding: fields land
//! at offsets that are multiples of their own alignment;
//! padding bytes are zeros (encoder writes them explicitly;
//! decoder ignores their contents). `Archived<T>` is the
//! `#[repr(C)]` tuple-struct counterpart of `T` so the validate-
//! then-transmute zero-copy path holds end-to-end.

use crate::{Archive, Deserialize, Error, Serialize, Writer};
use core::mem::{align_of, size_of};

// ============================================================
// Offset arithmetic.
// ============================================================

/// Round `offset` up to the next multiple of `align`. `align`
/// must be a power of two (Rust alignments always are).
pub(crate) const fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

// ============================================================
// Fixed-size arrays — `[T; N]`.
// ============================================================
//
// Rust's `[T; N]` is layout-stable: contiguous elements with no
// inter-element padding (every element's start is a multiple of
// `align_of::<T>()` because `size_of::<T>()` is itself a
// multiple of `align_of::<T>()` per Rust's layout rules).
//
// `Archived<[T; N]> = [T::Archived; N]`. Validate field-by-
// field, then transmute.

impl<T, const N: usize> Archive for [T; N]
where
    T: Archive,
    T::Archived: Sized,
{
    type Archived = [T::Archived; N];

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
        let elem_size = size_of::<T::Archived>();
        for i in 0..N {
            let off = i * elem_size;
            T::validate(&bytes[off..off + elem_size])?;
        }
        // SAFETY:
        // - length checked: bytes.len() == size_of::<[T::Archived; N]>()
        // - alignment checked: ptr is align_of::<...>()-aligned
        // - each element's bytes pass T::validate (above), so
        //   every element bit pattern is a valid T::Archived
        Ok(unsafe { &*(bytes.as_ptr() as *const Self::Archived) })
    }
}

impl<W, T, const N: usize> Serialize<W> for [T; N]
where
    W: Writer,
    T: Serialize<W>,
    T::Archived: Sized,
{
    fn serialize(&self, writer: &mut W) -> Result<(), Error> {
        // Each `T::serialize` already writes its full wire-size
        // (= `size_of::<T::Archived>()`, including trailing
        // padding). Concatenating N of them is the array's wire
        // form.
        for elem in self {
            elem.serialize(writer)?;
        }
        Ok(())
    }
}

impl<D, T, const N: usize> Deserialize<D> for [T; N]
where
    T: Deserialize<D>,
    T::Archived: Sized,
{
    fn deserialize(archived: &Self::Archived, ctx: &mut D) -> Result<Self, Error> {
        use core::mem::MaybeUninit;
        let mut out: [MaybeUninit<T>; N] = [const { MaybeUninit::uninit() }; N];
        for i in 0..N {
            match T::deserialize(&archived[i], ctx) {
                Ok(v) => {
                    out[i].write(v);
                }
                Err(e) => {
                    // SAFETY: only `out[0..i]` were initialised
                    // — drop them so we don't leak.
                    for slot in &mut out[..i] {
                        unsafe { slot.assume_init_drop() };
                    }
                    return Err(e);
                }
            }
        }
        // SAFETY: every slot was written.
        Ok(out.map(|m| unsafe { m.assume_init() }))
    }
}

// ============================================================
// Tuples 2..=8 — macro-generated.
// ============================================================

/// Internal macro generating one tuple's full Archive /
/// Serialize / Deserialize impl set.
///
/// `$arch` — name of the `Archived` `#[repr(C)]` tuple struct
/// (e.g. `ArchivedTuple2`).
/// `$arity` — element count (debug aid).
/// `($T:ident, $idx:tt)` — type-parameter ident + numeric
/// tuple-index per field.
macro_rules! impl_tuple {
    ($arch:ident, $arity:literal, $(($T:ident, $idx:tt)),+ $(,)?) => {
        /// `#[repr(C)]` tuple-struct archive of an N-tuple. Field
        /// access via `.0`, `.1`, … mirrors the source tuple.
        #[repr(C)]
        #[allow(missing_docs)] // numbered tuple fields don't need docstrings
        pub struct $arch<$($T,)+>(
            $(pub <$T as Archive>::Archived,)+
        )
        where
            $($T: Archive,)+
            $(<$T as Archive>::Archived: Sized,)+;

        impl<$($T,)+> Archive for ($($T,)+)
        where
            $($T: Archive,)+
            $(<$T as Archive>::Archived: Sized,)+
        {
            type Archived = $arch<$($T,)+>;

            fn validate(bytes: &[u8]) -> Result<&Self::Archived, Error> {
                let need = size_of::<Self::Archived>();
                if bytes.len() < need {
                    return Err(Error::Truncated);
                }
                if bytes.len() != need {
                    return Err(Error::BadLayout);
                }
                if !(bytes.as_ptr() as usize)
                    .is_multiple_of(align_of::<Self::Archived>())
                {
                    return Err(Error::BadAlignment);
                }
                // Walk each field; compute its offset by
                // `align_up` from the running cursor, then call
                // `validate` on its sub-slice.
                let mut _cursor: usize = 0;
                $(
                    _cursor = align_up(_cursor, align_of::<<$T as Archive>::Archived>());
                    let _size = size_of::<<$T as Archive>::Archived>();
                    $T::validate(&bytes[_cursor.._cursor + _size])?;
                    _cursor += _size;
                )+
                // SAFETY:
                // - `$arch` is `#[repr(C)]`, so its in-memory
                //   layout matches the SysV-ABI wire layout the
                //   field-validate loop just walked.
                // - length + alignment + per-field invariants
                //   all checked.
                Ok(unsafe { &*(bytes.as_ptr() as *const Self::Archived) })
            }
        }

        impl<W, $($T,)+> Serialize<W> for ($($T,)+)
        where
            W: Writer,
            $($T: Serialize<W>,)+
            $(<$T as Archive>::Archived: Sized,)+
        {
            fn serialize(&self, writer: &mut W) -> Result<(), Error> {
                let _start = writer.position();
                let mut _cursor: usize = 0;
                $(
                    // Pad to this field's alignment.
                    let want = align_up(_cursor, align_of::<<$T as Archive>::Archived>());
                    while _cursor < want {
                        writer.write_all(&[0u8])?;
                        _cursor += 1;
                    }
                    self.$idx.serialize(writer)?;
                    _cursor += size_of::<<$T as Archive>::Archived>();
                )+
                // Trailing padding to the tuple's own alignment
                // (so wire size matches `size_of::<Self::Archived>()`).
                let total = size_of::<Self::Archived>();
                while _cursor < total {
                    writer.write_all(&[0u8])?;
                    _cursor += 1;
                }
                debug_assert_eq!(writer.position() - _start, total);
                Ok(())
            }
        }

        impl<D, $($T,)+> Deserialize<D> for ($($T,)+)
        where
            $($T: Deserialize<D>,)+
            $(<$T as Archive>::Archived: Sized,)+
        {
            fn deserialize(archived: &Self::Archived, ctx: &mut D) -> Result<Self, Error> {
                Ok(($($T::deserialize(&archived.$idx, ctx)?,)+))
            }
        }
    };
}

impl_tuple!(ArchivedTuple2, 2, (T0, 0), (T1, 1));
impl_tuple!(ArchivedTuple3, 3, (T0, 0), (T1, 1), (T2, 2));
impl_tuple!(ArchivedTuple4, 4, (T0, 0), (T1, 1), (T2, 2), (T3, 3));
impl_tuple!(
    ArchivedTuple5,
    5,
    (T0, 0),
    (T1, 1),
    (T2, 2),
    (T3, 3),
    (T4, 4),
);
impl_tuple!(
    ArchivedTuple6,
    6,
    (T0, 0),
    (T1, 1),
    (T2, 2),
    (T3, 3),
    (T4, 4),
    (T5, 5),
);
impl_tuple!(
    ArchivedTuple7,
    7,
    (T0, 0),
    (T1, 1),
    (T2, 2),
    (T3, 3),
    (T4, 4),
    (T5, 5),
    (T6, 6),
);
impl_tuple!(
    ArchivedTuple8,
    8,
    (T0, 0),
    (T1, 1),
    (T2, 2),
    (T3, 3),
    (T4, 4),
    (T5, 5),
    (T6, 6),
    (T7, 7),
);

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_smoke() {
        assert_eq!(align_up(0, 1), 0);
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(3, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(7, 8), 8);
    }

    #[test]
    fn tuple_size_matches_repr_c() {
        // `(u32, u8)`: align 4, size 8 (4 + 1 + 3 pad).
        assert_eq!(size_of::<ArchivedTuple2<u32, u8>>(), 8);
        // `(u8, u32)`: align 4, size 8 (1 + 3 pad + 4).
        assert_eq!(size_of::<ArchivedTuple2<u8, u32>>(), 8);
        // `(u16, u32)`: align 4, size 8.
        assert_eq!(size_of::<ArchivedTuple2<u16, u32>>(), 8);
        // `(u8, u8)`: align 1, size 2.
        assert_eq!(size_of::<ArchivedTuple2<u8, u8>>(), 2);
    }
}
