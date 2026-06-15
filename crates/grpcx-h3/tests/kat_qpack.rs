//! QPACK static-table round-trip + reject coverage.
//!
//! v0.3 ships the static-only path; KAT focuses on the
//! end-to-end encode → decode shape rather than a literal-byte
//! match against a `nghttp3` capture, since byte-equal hex
//! matching only becomes meaningful once the encoder has Huffman
//! support (v1.x).

use grpcx_h3::{
    Error, Header, decode_field_section, encode_field_section, find_static, find_static_name,
    static_entry,
};

fn h(name: &[u8], value: &[u8]) -> Header {
    Header {
        name: name.to_vec(),
        value: value.to_vec(),
    }
}

fn round_trip(headers: Vec<Header>) {
    let mut wire = Vec::new();
    encode_field_section(&headers, &mut wire).expect("encode");
    let decoded = decode_field_section(&wire).expect("decode");
    assert_eq!(decoded, headers);
}

// ============================================================
// Round-trip cases.
// ============================================================

#[test]
fn request_header_section_round_trip() {
    // Typical request: 3 of 4 fields are exact static hits;
    // `:authority` is the one literal value.
    round_trip(vec![
        h(b":method", b"GET"),
        h(b":scheme", b"https"),
        h(b":path", b"/"),
        h(b":authority", b"example.com"),
    ]);
}

#[test]
fn response_header_section_round_trip() {
    round_trip(vec![
        h(b":status", b"200"),
        h(b"content-type", b"text/html; charset=utf-8"),
        h(b"x-custom-header", b"custom-value"),
    ]);
}

#[test]
fn all_indexed_round_trip() {
    // Every header is an exact static hit.
    round_trip(vec![
        h(b":method", b"GET"),
        h(b":scheme", b"https"),
        h(b":status", b"200"),
        h(b"accept", b"*/*"),
    ]);
}

#[test]
fn all_literal_round_trip() {
    // No static hits at all; both name and value literal.
    round_trip(vec![
        h(b"x-trace-id", b"deadbeef-cafe-feed-face-baadc0deddee"),
        h(b"x-region", b"ap-northeast-1"),
    ]);
}

#[test]
fn name_only_literal_value_round_trip() {
    // Name has a static hit (`cookie` at index 5); value is
    // literal — should go through Literal w/ Name Reference.
    round_trip(vec![h(b"cookie", b"session=abcdef; theme=dark")]);
}

#[test]
fn empty_field_section_round_trip() {
    round_trip(vec![]);
}

#[test]
fn long_value_round_trip() {
    // Force a multi-byte prefixed integer on the value length.
    let big = vec![b'x'; 200];
    round_trip(vec![h(b":path", &big)]);
}

#[test]
fn long_name_literal_round_trip() {
    // 3-bit prefix integer on the name length forces continuation
    // (name length > 6 must escape past the prefix).
    let name = b"x-extremely-long-header-name-that-spills-past-the-prefix";
    round_trip(vec![h(name, b"value")]);
}

#[test]
fn pseudo_then_regular_round_trip() {
    round_trip(vec![
        h(b":method", b"POST"),
        h(b":scheme", b"https"),
        h(b":path", b"/api/v1/items"),
        h(b":authority", b"api.example.com"),
        h(b"content-type", b"application/json"),
        h(b"content-length", b"42"),
        h(b"x-request-id", b"abc-123"),
    ]);
}

#[test]
fn static_hit_in_late_table_round_trip() {
    // `:status 421` is at static index 69 — past the 6-bit
    // prefix integer boundary, so the Indexed Field Line uses a
    // multi-byte prefixed-integer continuation.
    round_trip(vec![h(b":status", b"421")]);
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn non_zero_ric_rejected() {
    // RIC = 5 (1-byte form, value 5 fits the 8-bit prefix exactly).
    // Then Sign=0, Delta-Base=0.
    let wire = [0x05u8, 0x00];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn non_zero_delta_base_rejected() {
    // RIC=0, Sign=0, Delta-Base=3.
    let wire = [0x00u8, 0x03];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn signed_delta_base_rejected() {
    // RIC=0, Sign=1, Delta-Base=0.
    let wire = [0x00u8, 0x80];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn huffman_value_rejected() {
    // Valid prefix (0,0,0), then Literal w/ Literal Name with
    // an H=1 value string. Name = "x" (1 byte, H=0), Value
    // string starts with `1xxxxxxx` (H=1).
    let wire = [
        0x00u8,      // RIC=0
        0x00,        // S=0, Delta-Base=0
        0b0010_1001, // Literal w/ Literal Name, N=0, H=0, name-len=1
        b'x',
        0b1000_0001, // value: H=1, len=1
        b'A',
    ];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn out_of_range_static_index_rejected() {
    // Indexed Field Line: T=1 (static), index = 100 (past the
    // 99-entry table). Multi-byte prefix: max-prefix = 63;
    // continuation = 100 - 63 = 37.
    let wire = [
        0x00u8,      // RIC=0
        0x00,        // S=0, Delta-Base=0
        0b1111_1111, // Indexed, T=1, index prefix maxed
        37,          // continuation
    ];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn truncated_field_section_rejected() {
    // Just the first prefix byte; everything else missing.
    let wire = [0x00u8];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

#[test]
fn empty_buf_rejected() {
    assert_eq!(decode_field_section(&[]), Err(Error::QpackDecode));
}

#[test]
fn truncated_literal_value_rejected() {
    // Literal w/ Literal Name, name "ab" (len 2), value-length 5
    // declared but only 3 bytes present.
    let wire = [
        0x00u8,      // RIC=0
        0x00,        // S=0, Delta-Base=0
        0b0010_1010, // 001 N=0 H=0 name-len=2
        b'a',
        b'b',
        0b0000_0101, // H=0, value-len=5
        b'A',
        b'B',
        b'C',
    ];
    assert_eq!(decode_field_section(&wire), Err(Error::QpackDecode));
}

// ============================================================
// Static-table helper smoke (re-exports).
// ============================================================

#[test]
fn static_helpers_smoke() {
    assert_eq!(static_entry(17), Some((&b":method"[..], &b"GET"[..])));
    assert_eq!(find_static(b":scheme", b"https"), Some(23));
    assert_eq!(find_static_name(b":status"), Some(24));
    assert!(find_static(b"unknown-header", b"x").is_none());
    assert!(find_static_name(b"unknown-header").is_none());
}
