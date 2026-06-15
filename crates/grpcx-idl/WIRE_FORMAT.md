# grpcx-idl wire format

> **v0.2 status**: this spec covers the primitive types. v0.3
> adds composite types, v0.4 adds variable-length, v0.5 adds
> tagged unions. The numeric primitives + bool spec below is
> the wire-format ground truth from v0.2 onward.

## Endianness

All multi-byte values are **little-endian**. This matches every
relevant target CPU (x86_64, aarch64, riscv, wasm). Big-endian
targets are rejected at compile time via a `compile_error!`;
v1.x adds byteswap accessor wrappers if a real BE host project
appears.

## Alignment

Each archived type carries a compile-time alignment requirement
matching its native Rust `align_of`:

| Type | Wire size (bytes) | Align (bytes) | `Archived<T>` |
|---|---:|---:|---|
| `u8` / `i8` | 1 | 1 | `T` |
| `bool` | 1 | 1 | `bool` |
| `u16` / `i16` | 2 | 2 | `T` |
| `u32` / `i32` | 4 | 4 | `T` |
| `u64` / `i64` | 8 | 8 | `T` |
| `u128` / `i128` | 16 | 16 | `T` |
| `f32` | 4 | 4 | `f32` |
| `f64` | 8 | 8 | `f64` |

For primitive types, `Archived<T> = T` on little-endian targets —
the bytes on the wire are already the native representation, so
`from_bytes::<T>(&[u8]) -> &T` is just a length+alignment check
followed by a transmute. Cap'n-Proto-style segmentation lands
later for composite types where wire ≠ native layout.

## Padding

For primitives in isolation, no padding. For composite types
(struct / tuple / Vec of T), every field starts at a multiple
of its own alignment; padding bytes between fields are written
as zeros (and ignored on decode). This matches `#[repr(C)]`
struct layout under the SysV ABI.

## Primitive layout

### Unsigned integers
- `u8`: single byte; range `0..=255`.
- `u16`: two LE bytes; range `0..=65 535`.
- `u32`: four LE bytes.
- `u64`: eight LE bytes.
- `u128`: sixteen LE bytes.

### Signed integers
Two's-complement representation, same byte width as the matching
unsigned type. LE byte order.
- `i8`/`i16`/`i32`/`i64`/`i128`.

### Floats
IEEE 754 binary32 / binary64. Stored as the `to_le_bytes()`
representation. NaN / Inf round-trip unchanged.
- `f32`: four LE bytes.
- `f64`: eight LE bytes.

### Boolean
Single byte. **Only** `0x00` (false) and `0x01` (true) are
valid. Any other byte value triggers `Error::BadLayout` on
decode — this is the only primitive with a value-level
validation.

## Decoder validation policy

`from_bytes::<T>(bytes) -> Result<&Archived<T>, Error>` enforces:

1. **Length**: `bytes.len()` ≥ `size_of::<Archived<T>>()` for
   the primitive case (must equal for v0.2's single-value
   surface; composite v0.3+ relaxes to ≥).
2. **Alignment**: `bytes.as_ptr() as usize` must be a multiple
   of `align_of::<Archived<T>>()`. Returns
   `Error::BadAlignment` otherwise. (Note: `Vec<u8>::as_slice()`
   pointers from `Box`/system allocator are typically 16-byte
   aligned, so primitives ≤ `u128` work out of the box.)
3. **Type-level invariants**: `bool` byte ∈ `{0, 1}`. Other
   primitives have no value-level validation (all bit patterns
   are valid for `u*` / `i*` / `f*`).

If all three pass, the bytes are cast directly to
`&Archived<T>` with no copy.

## KAT vectors (selected, see `tests/kat_primitives.rs` for the
full set)

| Value | Type | Wire bytes |
|---|---|---|
| `0x01_u8` | `u8` | `[0x01]` |
| `0xFE_i8` (i.e. `-2`) | `i8` | `[0xFE]` |
| `true` | `bool` | `[0x01]` |
| `false` | `bool` | `[0x00]` |
| `0x0102_u16` | `u16` | `[0x02, 0x01]` |
| `-2_i16` | `i16` | `[0xFE, 0xFF]` |
| `0xDEAD_BEEF_u32` | `u32` | `[0xEF, 0xBE, 0xAD, 0xDE]` |
| `1.0_f32` | `f32` | `[0x00, 0x00, 0x80, 0x3F]` |
| `-2.0_f64` | `f64` | `[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0]` |

## Forward-compat reservations

- A future "narrow integer" extension may add varint encoding
  for small-value-dense fields. Until then there is no varint
  on the wire; every numeric primitive is fixed-width.
- A future "signed booleans" extension is **not** planned —
  `bool` stays single-byte 0/1.
- Composite + variable-length types (v0.3 / v0.4) layer on
  top of the primitive spec without changing this file's
  rules.
