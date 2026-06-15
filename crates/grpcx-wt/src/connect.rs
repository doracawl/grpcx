//! Extended CONNECT bootstrap — RFC 9220 §3 + RFC 8441 §4.
//!
//! Builds + validates the request header section for an
//! extended-CONNECT request that opens a WebTransport session.
//! The bootstrap exchange shape is:
//!
//! 1. Client sends an H/3 bidi-stream HEADERS frame with:
//!    - `:method = CONNECT`
//!    - `:protocol = webtransport`
//!    - `:scheme = https`
//!    - `:authority = <authority>`
//!    - `:path = <path>`
//! 2. Server responds with `:status = 200` (acceptance) or any
//!    `4xx` / `5xx` (rejection).
//!
//! The session id is the H/3 stream id of this CONNECT request
//! (RFC 9220 §3).

use crate::Error;
use alloc::vec;
use alloc::vec::Vec;
use grpcx_h3::Header;

/// Build the request header section for a WebTransport
/// extended-CONNECT request.
pub fn build_connect_request_headers(authority: &[u8], path: &[u8]) -> Vec<Header> {
    vec![
        h(b":method", b"CONNECT"),
        h(b":scheme", b"https"),
        h(b":protocol", b"webtransport"),
        h(b":authority", authority),
        h(b":path", path),
    ]
}

/// Build the response header section for an accepted CONNECT
/// (server emits this).
pub fn build_connect_response_headers() -> Vec<Header> {
    vec![h(b":status", b"200")]
}

/// Validate an incoming request header section as a WebTransport
/// extended-CONNECT request. Returns:
/// - `Ok((authority, path))` on success — owned `Vec<u8>` copies
///   of the relevant pseudo-headers (the caller may want them).
/// - `Err(Error::BadStreamFrame)` if any pseudo-header is
///   missing or wrong.
pub fn validate_connect_request(headers: &[Header]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut method: Option<&[u8]> = None;
    let mut scheme: Option<&[u8]> = None;
    let mut protocol: Option<&[u8]> = None;
    let mut authority: Option<&[u8]> = None;
    let mut path: Option<&[u8]> = None;
    for h in headers {
        match h.name.as_slice() {
            b":method" => method = Some(&h.value),
            b":scheme" => scheme = Some(&h.value),
            b":protocol" => protocol = Some(&h.value),
            b":authority" => authority = Some(&h.value),
            b":path" => path = Some(&h.value),
            _ => {}
        }
    }
    if method != Some(b"CONNECT") {
        return Err(Error::BadStreamFrame);
    }
    if protocol != Some(b"webtransport") {
        return Err(Error::BadStreamFrame);
    }
    let scheme = scheme.ok_or(Error::BadStreamFrame)?;
    if scheme != b"https" {
        return Err(Error::BadStreamFrame);
    }
    let authority = authority.ok_or(Error::BadStreamFrame)?;
    let path = path.ok_or(Error::BadStreamFrame)?;
    Ok((authority.to_vec(), path.to_vec()))
}

/// Extract the `:status` pseudo-header value (parsed as `u16`)
/// from a response header section. Returns
/// `Err(Error::BadStreamFrame)` if `:status` is missing or
/// malformed.
pub fn parse_response_status(headers: &[Header]) -> Result<u16, Error> {
    for h in headers {
        if h.name.as_slice() == b":status" {
            // The value is ASCII decimal.
            return core::str::from_utf8(&h.value)
                .map_err(|_| Error::BadStreamFrame)?
                .parse::<u16>()
                .map_err(|_| Error::BadStreamFrame);
        }
    }
    Err(Error::BadStreamFrame)
}

fn h(n: &[u8], v: &[u8]) -> Header {
    Header {
        name: n.to_vec(),
        value: v.to_vec(),
    }
}
