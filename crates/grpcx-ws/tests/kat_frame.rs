//! v0.2 KAT — frame codec via the public `encode_frame` /
//! `decode_frame` entry points.
//!
//! Each case exercises the RFC 6455 § 5.2 byte layout: per-
//! opcode round-trip, the three length-encoding forms,
//! masking XOR, and reject paths (reserved opcode / RSV
//! bit / overlong length / truncations).

use grpcx_ws::{Frame, Opcode, WsError, decode_frame, encode_frame};

fn rt(frame: Frame) -> Frame {
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);
    let (decoded, consumed) = decode_frame(&wire).unwrap();
    assert_eq!(consumed, wire.len(), "decode must consume the full frame");
    decoded
}

// ============================================================
// Opcode round-trip per supported opcode.
// ============================================================

#[test]
fn round_trip_continuation() {
    let f = Frame {
        fin: false,
        opcode: Opcode::Continuation,
        payload: b"chunk".to_vec(),
        ..Frame::default()
    };
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn round_trip_text() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Text,
        payload: b"hello world".to_vec(),
        ..Frame::default()
    };
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn round_trip_binary() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Binary,
        payload: vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF],
        ..Frame::default()
    };
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn round_trip_close_ping_pong() {
    for op in [Opcode::Close, Opcode::Ping, Opcode::Pong] {
        let f = Frame {
            fin: true,
            opcode: op,
            payload: vec![0x03, 0xE8], // 1000 BE — close-code-ish
            ..Frame::default()
        };
        assert_eq!(rt(f.clone()), f);
    }
}

// ============================================================
// Length encoding boundaries.
// ============================================================

#[test]
fn length_zero() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Text,
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    // 2 bytes: 0x81 (FIN + Text), 0x00 (no mask, len=0).
    assert_eq!(wire, [0x81, 0x00]);
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn length_125_short_form() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Binary,
        payload: vec![0xAA; 125],
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    // Header bytes: 0x82 (FIN + Binary), 0x7D (no mask, len=125 = 0x7D).
    assert_eq!(wire[..2], [0x82, 0x7D]);
    assert_eq!(wire.len(), 2 + 125);
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn length_126_uses_extended_16bit() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Binary,
        payload: vec![0xCC; 126],
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    // Bytes: 0x82, 0x7E, 0x00 0x7E (len=126 BE).
    assert_eq!(wire[..4], [0x82, 0x7E, 0x00, 0x7E]);
    assert_eq!(wire.len(), 4 + 126);
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn length_0xffff_extended_16bit_upper_bound() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Binary,
        payload: vec![0xDD; 0xFFFF],
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    assert_eq!(wire[..4], [0x82, 0x7E, 0xFF, 0xFF]);
    assert_eq!(wire.len(), 4 + 0xFFFF);
    assert_eq!(rt(f.clone()), f);
}

#[test]
fn length_0x10000_uses_extended_64bit() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Binary,
        payload: vec![0xEE; 0x10000],
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    // Bytes: 0x82, 0x7F, 8-byte BE = 0x0000_0000_0001_0000.
    assert_eq!(
        wire[..10],
        [0x82, 0x7F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
    );
    assert_eq!(wire.len(), 10 + 0x10000);
    assert_eq!(rt(f.clone()), f);
}

// ============================================================
// Masking round-trip.
// ============================================================

#[test]
fn masking_xor_round_trip_text() {
    let key = [0x37, 0xFA, 0x21, 0x3D];
    let f = Frame {
        fin: true,
        opcode: Opcode::Text,
        mask: Some(key),
        payload: b"Hello".to_vec(),
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    // Per RFC 6455 § 5.7, masked "Hello" → 7F 9F 4D 51 58.
    assert_eq!(
        wire,
        [
            0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, // header + mask key
            0x7F, 0x9F, 0x4D, 0x51, 0x58, // masked payload
        ]
    );
    let decoded = rt(f.clone());
    // After decode, `payload` is back to the unmasked form.
    assert_eq!(decoded.payload, b"Hello");
    assert_eq!(decoded.mask, Some(key));
}

#[test]
fn masking_empty_payload() {
    // Mask key still appears on the wire even when the
    // payload is empty.
    let key = [0x12, 0x34, 0x56, 0x78];
    let f = Frame {
        fin: true,
        opcode: Opcode::Ping,
        mask: Some(key),
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    assert_eq!(wire, [0x89, 0x80, 0x12, 0x34, 0x56, 0x78]);
    assert_eq!(rt(f.clone()), f);
}

// ============================================================
// Reject paths.
// ============================================================

#[test]
fn reject_reserved_opcode() {
    // Reserved non-control 0x3 with FIN=1.
    let wire = [0x83, 0x00];
    let err = decode_frame(&wire).unwrap_err();
    assert_eq!(err, WsError::InvalidOpcode(0x3));
    // Reserved control 0xF.
    let wire = [0x8F, 0x00];
    let err = decode_frame(&wire).unwrap_err();
    assert_eq!(err, WsError::InvalidOpcode(0xF));
}

#[test]
fn reject_rsv_bits_set() {
    // FIN + RSV1 + Text (0xC1) with no payload.
    let wire = [0xC1, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::ReservedBitSet);
    // RSV2.
    let wire = [0xA1, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::ReservedBitSet);
    // RSV3.
    let wire = [0x91, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::ReservedBitSet);
}

#[test]
fn reject_overlong_length_126() {
    // Length-126 with value=0 → overlong (≤ 125 must use
    // the short form).
    let wire = [0x82, 0x7E, 0x00, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::PayloadTooLarge);
    // Length-126 with value=125 → still overlong.
    let wire = [0x82, 0x7E, 0x00, 0x7D];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::PayloadTooLarge);
}

#[test]
fn reject_overlong_length_127() {
    // Length-127 with value=0xFFFF → overlong (≤ 0xFFFF
    // must use the 16-bit form).
    let wire = [0x82, 0x7F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::PayloadTooLarge);
}

#[test]
fn reject_64bit_length_msb_set() {
    let wire = [0x82, 0x7F, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::PayloadTooLarge);
}

#[test]
fn reject_truncated_header() {
    assert_eq!(decode_frame(&[]).unwrap_err(), WsError::Truncated);
    assert_eq!(decode_frame(&[0x81]).unwrap_err(), WsError::Truncated);
}

#[test]
fn reject_truncated_extended_length_16bit() {
    // Length-126 with only 1 of the 2 extra length bytes.
    let wire = [0x82, 0x7E, 0x00];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::Truncated);
}

#[test]
fn reject_truncated_mask_key() {
    // MASK=1, short-form length=0, but only 3 of the 4 key
    // bytes.
    let wire = [0x81, 0x80, 0x01, 0x02, 0x03];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::Truncated);
}

#[test]
fn reject_truncated_payload() {
    // Length=5 short-form, but only 3 payload bytes.
    let wire = [0x81, 0x05, b'h', b'e', b'y'];
    assert_eq!(decode_frame(&wire).unwrap_err(), WsError::Truncated);
}

// ============================================================
// Trailing bytes: decode returns consumed-count so caller
// can re-enter with the remainder.
// ============================================================

#[test]
fn trailing_bytes_are_left_for_caller() {
    let f = Frame {
        fin: true,
        opcode: Opcode::Text,
        payload: b"ok".to_vec(),
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    wire.extend_from_slice(b"NEXT_FRAME_BYTES");
    let (decoded, consumed) = decode_frame(&wire).unwrap();
    assert_eq!(decoded.payload, b"ok");
    // Frame is 4 bytes (0x81, 0x02, 'o', 'k'); rest belongs
    // to the next frame.
    assert_eq!(consumed, 4);
    assert_eq!(&wire[consumed..], b"NEXT_FRAME_BYTES");
}
