//! Fuzz-only internals (unstable, `#[doc(hidden)]`). Drivers
//! that the cargo-fuzz subcrate calls into. NOT a stable API.

use crate::{
    DynamicTable, decode_decoder_instruction, decode_encoder_instruction, decode_field_section,
    decode_field_section_with_table, decode_frame, encode_frame, read_stream_type,
};
use alloc::vec::Vec;

extern crate std;

/// Decode → re-encode → decode round-trip on a frame.
/// **Invariant**: no input must produce a panic.
pub fn drive_frame(data: &[u8]) {
    let mut cursor = data;
    let Ok(f) = decode_frame(&mut cursor) else {
        return;
    };
    let mut out: Vec<u8> = Vec::new();
    let _ = encode_frame(&f, &mut out);
    if !out.is_empty() {
        let mut cursor2: &[u8] = &out;
        let _ = decode_frame(&mut cursor2);
    }
}

/// Decode a field section (static-only path).
/// **Invariant**: no input must panic.
pub fn drive_field_section_static(data: &[u8]) {
    let _ = decode_field_section(data);
}

/// Decode a field section against an empty dynamic table.
/// **Invariant**: no input must panic.
pub fn drive_field_section_dynamic(data: &[u8]) {
    let table = DynamicTable::new(4096);
    let _ = decode_field_section_with_table(data, &table);
}

/// Decode an encoder-stream instruction. **Invariant**: no
/// input must panic.
pub fn drive_encoder_instruction(data: &[u8]) {
    let mut cursor = data;
    while !cursor.is_empty() {
        if decode_encoder_instruction(&mut cursor).is_err() {
            break;
        }
    }
}

/// Decode a decoder-stream instruction. **Invariant**: no
/// input must panic.
pub fn drive_decoder_instruction(data: &[u8]) {
    let mut cursor = data;
    while !cursor.is_empty() {
        if decode_decoder_instruction(&mut cursor).is_err() {
            break;
        }
    }
}

/// Read a uni-stream type discriminator. **Invariant**: no
/// input must panic.
pub fn drive_stream_type(data: &[u8]) {
    let _ = read_stream_type(data);
}
