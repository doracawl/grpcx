//! KAT round-trip + reject coverage for the HTTP/3 frame codec.
//!
//! One round-trip per RFC 9114 §7.2 frame variant + a handful of
//! reject paths (truncated length, malformed payload, reserved
//! type forward-skip).

use grpcx_h3::{Error, Frame, PriorityKind, SettingPair, decode_frame, encode_frame};

fn round_trip(f: Frame) {
    let mut out = Vec::new();
    encode_frame(&f, &mut out).expect("encode");
    let mut cursor: &[u8] = &out;
    let decoded = decode_frame(&mut cursor).expect("decode");
    assert!(cursor.is_empty(), "cursor must be drained");
    assert_eq!(decoded, f);
}

// ============================================================
// One round-trip per variant.
// ============================================================

#[test]
fn data_round_trip() {
    round_trip(Frame::Data {
        payload: b"hello world".to_vec(),
    });
}

#[test]
fn data_empty_round_trip() {
    round_trip(Frame::Data { payload: vec![] });
}

#[test]
fn headers_round_trip() {
    round_trip(Frame::Headers {
        encoded_field_section: vec![0x00, 0x00, 0xc0, 0xc1, 0x5f, 0x52],
    });
}

#[test]
fn cancel_push_round_trip() {
    round_trip(Frame::CancelPush { push_id: 42 });
    round_trip(Frame::CancelPush {
        push_id: 1 << 30, // 4-byte varint boundary
    });
}

#[test]
fn settings_round_trip() {
    round_trip(Frame::Settings(vec![
        SettingPair {
            id: 0x06, // SETTINGS_MAX_FIELD_SECTION_SIZE
            value: 16 * 1024,
        },
        SettingPair {
            id: 0x01, // SETTINGS_QPACK_MAX_TABLE_CAPACITY
            value: 4096,
        },
        SettingPair {
            id: 0x07, // SETTINGS_QPACK_BLOCKED_STREAMS
            value: 100,
        },
    ]));
}

#[test]
fn settings_empty_round_trip() {
    round_trip(Frame::Settings(vec![]));
}

#[test]
fn push_promise_round_trip() {
    round_trip(Frame::PushPromise {
        push_id: 17,
        encoded_field_section: vec![0xde, 0xad, 0xbe, 0xef],
    });
}

#[test]
fn goaway_round_trip() {
    round_trip(Frame::Goaway {
        stream_or_push_id: 100,
    });
}

#[test]
fn max_push_id_round_trip() {
    round_trip(Frame::MaxPushId { push_id: 1000 });
}

#[test]
fn origin_round_trip() {
    round_trip(Frame::Origin {
        entries: vec![
            b"https://example.com".to_vec(),
            b"https://another.example".to_vec(),
        ],
    });
}

#[test]
fn origin_empty_round_trip() {
    round_trip(Frame::Origin { entries: vec![] });
}

#[test]
fn priority_update_request_round_trip() {
    round_trip(Frame::PriorityUpdate {
        kind: PriorityKind::Request,
        prioritized_id: 7,
        priority_field_value: b"u=1, i".to_vec(),
    });
}

#[test]
fn priority_update_push_round_trip() {
    round_trip(Frame::PriorityUpdate {
        kind: PriorityKind::Push,
        prioritized_id: 9,
        priority_field_value: b"u=3".to_vec(),
    });
}

#[test]
fn reserved_round_trip() {
    // An unknown frame type (e.g. 0x21) must decode to the
    // forward-compat `Reserved` variant, carrying the payload
    // verbatim. Encoding it back produces the same bytes.
    round_trip(Frame::Reserved {
        type_: 0x21,
        payload: vec![1, 2, 3, 4, 5],
    });
}

#[test]
fn reserved_for_rfc9114_reserved_codes() {
    // RFC 9114 §11.2.1 reserves 0x02 / 0x06 / 0x08 / 0x09. The
    // codec surfaces them as `Reserved`; v0.5's connection state
    // machine adds the close-with-error rule when they appear on
    // the control stream.
    for type_ in [0x02u64, 0x06, 0x08, 0x09] {
        round_trip(Frame::Reserved {
            type_,
            payload: vec![],
        });
    }
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn truncated_length_rejected() {
    // Type=DATA, declared length=10, but only 4 payload bytes.
    let mut buf: &[u8] = &[0x00, 0x0a, 0x01, 0x02, 0x03, 0x04];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn truncated_type_rejected() {
    let mut buf: &[u8] = &[];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn truncated_length_varint_rejected() {
    // Only the type byte; no length varint at all.
    let mut buf: &[u8] = &[0x00];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn settings_odd_pair_rejected() {
    // SETTINGS frame with one varint in the payload (id only,
    // no value).
    let mut buf: &[u8] = &[0x04, 0x01, 0x06];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn cancel_push_with_trailing_garbage_rejected() {
    // CANCEL_PUSH has an exact-fit varint payload; trailing
    // bytes are malformed.
    let mut buf: &[u8] = &[0x03, 0x02, 0x05, 0xff];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn goaway_with_trailing_garbage_rejected() {
    let mut buf: &[u8] = &[0x07, 0x02, 0x05, 0xff];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn max_push_id_with_trailing_garbage_rejected() {
    let mut buf: &[u8] = &[0x0d, 0x02, 0x05, 0xff];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn origin_truncated_entry_rejected() {
    // ORIGIN: one entry, length=5, only 3 bytes of payload.
    let mut buf: &[u8] = &[0x0c, 0x05, 0x00, 0x05, b'a', b'b', b'c'];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn origin_one_byte_payload_rejected() {
    // ORIGIN must have a 2-byte length prefix before any entry.
    let mut buf: &[u8] = &[0x0c, 0x01, 0xff];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

// ============================================================
// Streaming: two frames back-to-back decode independently.
// ============================================================

#[test]
fn back_to_back_frames_decode() {
    let mut out = Vec::new();
    encode_frame(
        &Frame::Headers {
            encoded_field_section: vec![0xc0, 0xc1],
        },
        &mut out,
    )
    .unwrap();
    encode_frame(
        &Frame::Data {
            payload: b"body".to_vec(),
        },
        &mut out,
    )
    .unwrap();

    let mut cursor: &[u8] = &out;
    let first = decode_frame(&mut cursor).unwrap();
    let second = decode_frame(&mut cursor).unwrap();
    assert!(cursor.is_empty());
    assert!(matches!(first, Frame::Headers { .. }));
    assert!(matches!(second, Frame::Data { .. }));
}

#[test]
fn varint_len_helper() {
    use grpcx_h3::frame_varint_len;
    assert_eq!(frame_varint_len(0), 1);
    assert_eq!(frame_varint_len(63), 1);
    assert_eq!(frame_varint_len(64), 2);
    assert_eq!(frame_varint_len(16_383), 2);
    assert_eq!(frame_varint_len(16_384), 4);
    assert_eq!(frame_varint_len(1 << 30), 8);
}
