//! Fuzz-only internals (unstable, `#[doc(hidden)]`).
//! Drivers the cargo-fuzz subcrate calls into. NOT a
//! stable API.

use crate::{Metadata, decode_all_frames, decode_frame, try_decode_frame};
use alloc::string::String;
use alloc::vec::Vec;

/// Decode a single gRPC length-prefixed frame.
/// **Invariant**: no input must panic.
pub fn drive_frame(data: &[u8]) {
    let _ = decode_frame(data);
    let _ = try_decode_frame(data);
    let _ = decode_all_frames(data);
}

/// Parse request headers from an arbitrary metadata bag.
/// **Invariant**: no input must panic.
pub fn drive_request_headers(pairs: &[(String, Vec<u8>)]) {
    let meta = Metadata {
        headers: pairs.to_vec(),
    };
    let _ = crate::parse_request_headers(&meta);
    let _ = crate::parse_trailers(&meta);
}

/// Percent-decode arbitrary bytes.
/// **Invariant**: no input must panic; encode(decode(x))
/// must not panic either.
pub fn drive_percent(data: &[u8]) {
    let s = crate::percent_decode_lossy(data);
    let _ = crate::percent_encode(s.as_bytes());
}

/// Decode ws-mux frames + header lists.
/// **Invariant**: no input must panic.
#[cfg(feature = "transport-ws-mux")]
pub fn drive_mux(data: &[u8]) {
    if let Ok((_, _, _, payload)) = crate::decode_mux_frame(data) {
        let _ = crate::decode_header_list(payload);
    }
    let _ = crate::decode_header_list(data);
}
