//! WT stream prefix + datagram framing KAT.

use grpcx_wt::{
    Error, WT_STREAM_TYPE, read_wt_datagram, read_wt_stream_prefix, write_wt_datagram,
    write_wt_stream_prefix,
};

// ============================================================
// Stream prefix round-trips.
// ============================================================

#[test]
fn stream_prefix_round_trip_small_sid() {
    let mut out = Vec::new();
    // session_id = 0 — first client-initiated bidi.
    write_wt_stream_prefix(0, &mut out).unwrap();
    // Type byte value 0x54 = 84 exceeds the 1-byte varint range
    // (0..=63), so it lands as the 2-byte form `[0x40, 0x54]`.
    // Then session_id = 0 is the 1-byte form `[0x00]`.
    assert_eq!(out, vec![0x40, 0x54, 0x00]);
    let (sid, consumed) = read_wt_stream_prefix(&out).unwrap();
    assert_eq!(sid, 0);
    assert_eq!(consumed, out.len());
}

#[test]
fn stream_prefix_round_trip_2byte_sid() {
    let mut out = Vec::new();
    write_wt_stream_prefix(0x100, &mut out).unwrap();
    let (sid, consumed) = read_wt_stream_prefix(&out).unwrap();
    assert_eq!(sid, 0x100);
    assert_eq!(consumed, out.len());
}

#[test]
fn stream_prefix_round_trip_4byte_sid() {
    let mut out = Vec::new();
    write_wt_stream_prefix(1 << 20, &mut out).unwrap();
    let (sid, consumed) = read_wt_stream_prefix(&out).unwrap();
    assert_eq!(sid, 1 << 20);
    assert_eq!(consumed, out.len());
}

#[test]
fn stream_prefix_round_trip_with_trailing_payload() {
    let mut buf = Vec::new();
    write_wt_stream_prefix(4, &mut buf).unwrap();
    buf.extend_from_slice(b"hello world");
    let (sid, consumed) = read_wt_stream_prefix(&buf).unwrap();
    assert_eq!(sid, 4);
    // The caller drains exactly `consumed` to land on the user
    // bytes.
    assert_eq!(&buf[consumed..], b"hello world");
}

// ============================================================
// Stream prefix reject paths.
// ============================================================

#[test]
fn stream_prefix_empty_rejected() {
    assert!(read_wt_stream_prefix(&[]).is_err());
}

#[test]
fn stream_prefix_wrong_type_byte_rejected() {
    // 0x00 = some H/3 control-stream type; not WT.
    let buf = [0x00u8, 0x00];
    assert_eq!(read_wt_stream_prefix(&buf), Err(Error::BadStreamFrame));
}

#[test]
fn stream_prefix_truncated_after_type_rejected() {
    // Type byte but no session-id varint.
    let buf = [0x54u8];
    assert_eq!(read_wt_stream_prefix(&buf), Err(Error::BadStreamFrame));
}

#[test]
fn wt_stream_type_constant_is_0x54() {
    assert_eq!(WT_STREAM_TYPE, 0x54);
}

// ============================================================
// Datagram round-trips.
// ============================================================

#[test]
fn datagram_round_trip_small_sid_empty_payload() {
    let mut out = Vec::new();
    write_wt_datagram(0, b"", &mut out).unwrap();
    // quarter=0 → 1-byte varint.
    assert_eq!(out, vec![0x00]);
    let (sid, payload) = read_wt_datagram(&out).unwrap();
    assert_eq!(sid, 0);
    assert_eq!(payload, b"");
}

#[test]
fn datagram_round_trip_with_payload() {
    let mut out = Vec::new();
    write_wt_datagram(4, b"ping", &mut out).unwrap();
    let (sid, payload) = read_wt_datagram(&out).unwrap();
    assert_eq!(sid, 4);
    assert_eq!(payload, b"ping");
}

#[test]
fn datagram_round_trip_large_sid() {
    let mut out = Vec::new();
    // session_id = 4 * (1 << 20) → quarter = 1 << 20 → 4-byte varint.
    let sid = 4u64 << 20;
    write_wt_datagram(sid, b"x", &mut out).unwrap();
    let (sid2, payload) = read_wt_datagram(&out).unwrap();
    assert_eq!(sid2, sid);
    assert_eq!(payload, b"x");
}

// ============================================================
// Datagram reject paths.
// ============================================================

#[test]
fn datagram_unaligned_sid_rejected_on_encode() {
    // session_id not a multiple of 4 → can't form a clean
    // quarter-stream-id.
    let mut out = Vec::new();
    assert_eq!(write_wt_datagram(1, b"", &mut out), Err(Error::BadDatagram));
    assert_eq!(write_wt_datagram(2, b"", &mut out), Err(Error::BadDatagram));
    assert_eq!(write_wt_datagram(3, b"", &mut out), Err(Error::BadDatagram));
}

#[test]
fn datagram_empty_buf_rejected() {
    assert_eq!(read_wt_datagram(&[]), Err(Error::BadDatagram));
}
