//! Fuzz-only internals (unstable, `#[doc(hidden)]`). The fuzz
//! targets live in the `fuzz/` subcrate and can only see the
//! public surface, so the byte-driven op loops live here. NOT a
//! stable API; may change or vanish without notice.

use crate::{
    Frame, PacketHeader, decode_frame, decode_header, encode_frame, encode_header,
    frame_varint_len, varint_field_len,
};
use alloc::vec::Vec;

extern crate std;

/// Drive the varint helpers (`frame_varint_len` + `varint_field_len`)
/// with arbitrary input. The byte script: first 8 bytes (or fewer)
/// form a big-endian u64 that gets pushed through both helpers.
///
/// **Invariant**: no input must panic; both helpers are pure
/// length-classifying functions.
pub fn drive_varint(data: &[u8]) {
    let mut padded = [0u8; 8];
    let n = core::cmp::min(data.len(), 8);
    padded[..n].copy_from_slice(&data[..n]);
    let value = u64::from_be_bytes(padded);

    // Both helpers must agree (they are aliases for the same logic).
    let a = frame_varint_len(value);
    let b = varint_field_len(value);
    debug_assert_eq!(a, b);
    debug_assert!(matches!(a, 1 | 2 | 4 | 8));
}

/// Drive `decode_frame` followed by `encode_frame` on the same
/// input. If decoding succeeds, encoding the result back must
/// succeed too and never panic. If decoding fails we still must
/// not panic.
///
/// **Invariant**: no input must panic.
pub fn drive_frame(data: &[u8]) {
    let mut cursor = data;
    let Ok(f) = decode_frame(&mut cursor) else {
        return;
    };
    let mut out: Vec<u8> = Vec::new();
    let _ = encode_frame(&f, &mut out);
    // Re-decoding our own encoding must round-trip (when encoding
    // succeeded). We don't assert equality here because the encoder
    // is allowed to normalise (e.g. always emit the length-prefixed
    // Datagram form 0x31 instead of the no-length 0x30 the decoder
    // accepts).
    if !out.is_empty() {
        let mut cursor2: &[u8] = &out;
        let _ = decode_frame(&mut cursor2);
    }
}

/// Drive `decode_header` then re-encode. First byte of the input
/// picks the DCID length (0-20 for short headers; ignored for long
/// headers where the DCID length is on the wire).
///
/// **Invariant**: no input must panic.
pub fn drive_packet_header(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let dcid_len = (data[0] as usize) % 21; // 0..=20
    let rest = &data[1..];
    let Ok((hdr, _consumed)) = decode_header(rest, dcid_len) else {
        return;
    };
    let mut out: Vec<u8> = Vec::new();
    let _ = encode_header(&hdr, &mut out);
    // Round-trip the encoded form.
    if !out.is_empty() {
        let probe_dcid_len = match &hdr {
            PacketHeader::Short(s) => s.dcid.len as usize,
            PacketHeader::Long(_) => 0,
        };
        let _ = decode_header(&out, probe_dcid_len);
    }
    // Silence unused-variant warning if Frame ever changes shape.
    let _ = core::mem::size_of::<Frame>();
}
