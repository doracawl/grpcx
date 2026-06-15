//! RFC 9114 §6.2 unidirectional-stream type codec — KAT round-trip
//! + reject coverage.

use grpcx_h3::{
    Error, StreamType, is_critical, is_unique_per_direction, read_stream_type, write_stream_type,
};

fn round_trip(t: StreamType) {
    let mut out = Vec::new();
    let n_written = write_stream_type(t, &mut out).expect("write");
    assert_eq!(n_written, out.len());
    let (got, n_read) = read_stream_type(&out).expect("read");
    assert_eq!(got, t);
    assert_eq!(n_read, out.len());
}

#[test]
fn control_round_trip() {
    round_trip(StreamType::Control);
}

#[test]
fn push_round_trip() {
    round_trip(StreamType::Push);
}

#[test]
fn qpack_encoder_round_trip() {
    round_trip(StreamType::QpackEncoder);
}

#[test]
fn qpack_decoder_round_trip() {
    round_trip(StreamType::QpackDecoder);
}

#[test]
fn reserved_grease_round_trip() {
    // RFC 9114 §11.2.2 greasing: 0x1f * N + 0x21. First three
    // grease values.
    for n in 0..3u64 {
        let raw = 0x1f * n + 0x21;
        round_trip(StreamType::Reserved(raw));
    }
}

#[test]
fn reserved_big_varint_round_trip() {
    // Force a 4-byte varint encoding.
    round_trip(StreamType::Reserved(1 << 16));
    // And an 8-byte varint encoding.
    round_trip(StreamType::Reserved(1u64 << 35));
}

#[test]
fn empty_buf_rejected() {
    assert!(matches!(
        read_stream_type(&[]),
        Err(Error::BadFrame | Error::QpackDecode | Error::Internal)
    ));
}

#[test]
fn truncated_long_varint_rejected() {
    // Top two bits = 11 → 8-byte varint, but only 1 byte present.
    let buf = [0xc0u8];
    assert!(read_stream_type(&buf).is_err());
}

#[test]
fn classify_known_codes_smoke() {
    // First-byte encodings of the four known codes (1-byte form).
    let cases = [
        (0x00u8, StreamType::Control),
        (0x01, StreamType::Push),
        (0x02, StreamType::QpackEncoder),
        (0x03, StreamType::QpackDecoder),
    ];
    for (byte, expected) in cases {
        let buf = [byte];
        let (got, n) = read_stream_type(&buf).unwrap();
        assert_eq!(got, expected);
        assert_eq!(n, 1);
    }
}

#[test]
fn critical_classification_smoke() {
    assert!(is_critical(StreamType::Control));
    assert!(is_critical(StreamType::QpackEncoder));
    assert!(is_critical(StreamType::QpackDecoder));
    assert!(!is_critical(StreamType::Push));
    assert!(!is_critical(StreamType::Reserved(0x21)));
}

#[test]
fn unique_per_direction_classification_smoke() {
    assert!(is_unique_per_direction(StreamType::Control));
    assert!(is_unique_per_direction(StreamType::QpackEncoder));
    assert!(is_unique_per_direction(StreamType::QpackDecoder));
    assert!(!is_unique_per_direction(StreamType::Push));
    assert!(!is_unique_per_direction(StreamType::Reserved(0x21)));
}
