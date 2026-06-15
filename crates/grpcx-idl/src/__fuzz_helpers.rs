//! Fuzz-only internals (unstable, `#[doc(hidden)]`). NOT a
//! stable API.

use crate::{Archive, from_bytes};
use alloc::vec::Vec;

/// Drive `from_bytes::<T>` on adversarial input. Invariant:
/// no input must panic; either `Ok(...)` or `Err(...)` for any
/// byte slice.
fn drive<T: Archive>(data: &[u8]) {
    // Use a heap-allocated buffer so the start address is
    // ≥ 8-aligned in practice on every supported target — the
    // L7 wire format's alignment baseline is 4 for varlen and
    // up to 16 for u128 fields. Stack `&[u8]` would force-fail
    // alignment for higher-aligned types.
    let buf: Vec<u8> = data.to_vec();
    let _ = from_bytes::<T>(&buf);
}

/// Fuzz the primitive validate path.
pub fn drive_primitive(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    match data[0] % 5 {
        0 => drive::<u32>(&data[1..]),
        1 => drive::<u64>(&data[1..]),
        2 => drive::<i32>(&data[1..]),
        3 => drive::<bool>(&data[1..]),
        _ => drive::<f64>(&data[1..]),
    }
}

/// Fuzz the composite (tuple + array) validate path.
pub fn drive_composite(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    match data[0] % 4 {
        0 => drive::<(u32, u8)>(&data[1..]),
        1 => drive::<(u8, u32, u16)>(&data[1..]),
        2 => drive::<[u32; 4]>(&data[1..]),
        _ => drive::<[bool; 8]>(&data[1..]),
    }
}

/// Fuzz `Vec<u8>` / `String` validate.
pub fn drive_varlen(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    if data[0] & 1 == 0 {
        drive::<Vec<u8>>(&data[1..]);
    } else {
        drive::<alloc::string::String>(&data[1..]);
    }
}

/// Fuzz tagged-union validate (`Option` + `Result`).
pub fn drive_tagged(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    match data[0] % 4 {
        0 => drive::<Option<u32>>(&data[1..]),
        1 => drive::<Option<bool>>(&data[1..]),
        2 => drive::<Result<u32, u32>>(&data[1..]),
        _ => drive::<Result<u8, u64>>(&data[1..]),
    }
}
