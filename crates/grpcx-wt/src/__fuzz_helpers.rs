//! Fuzz-only internals (unstable, `#[doc(hidden)]`). Drivers
//! that the cargo-fuzz subcrate calls into. NOT a stable API.

use crate::capsule::decode_capsule;
use crate::connect::{parse_response_status, validate_connect_request};
use crate::framing::{read_wt_datagram, read_wt_stream_prefix};
use alloc::vec::Vec;
use grpcx_h3::Header;

extern crate std;

/// Parse a WT stream prefix off raw bytes.
/// **Invariant**: no input must panic.
pub fn drive_wt_stream_prefix(data: &[u8]) {
    let _ = read_wt_stream_prefix(data);
}

/// Parse a WT datagram off raw bytes.
/// **Invariant**: no input must panic.
pub fn drive_wt_datagram(data: &[u8]) {
    let _ = read_wt_datagram(data);
}

/// Decode a single capsule off raw bytes.
/// **Invariant**: no input must panic.
pub fn drive_capsule(data: &[u8]) {
    let mut cursor = data;
    let _ = decode_capsule(&mut cursor);
}

/// Validate a CONNECT-request header set built from raw bytes
/// interpreted as `name=value` pairs split on `\0`.
/// **Invariant**: no input must panic.
pub fn drive_connect_request(data: &[u8]) {
    let mut headers: Vec<Header> = Vec::new();
    for chunk in data.split(|b| *b == 0) {
        if let Some(eq) = chunk.iter().position(|b| *b == b'=') {
            headers.push(Header {
                name: chunk[..eq].to_vec(),
                value: chunk[eq + 1..].to_vec(),
            });
        }
    }
    let _ = validate_connect_request(&headers);
}

/// Parse the `:status` header out of an arbitrary header set
/// (single-header form synthesised from the input bytes).
/// **Invariant**: no input must panic.
pub fn drive_response_status(data: &[u8]) {
    let headers = std::vec![Header {
        name: b":status".to_vec(),
        value: data.to_vec(),
    }];
    let _ = parse_response_status(&headers);
}
