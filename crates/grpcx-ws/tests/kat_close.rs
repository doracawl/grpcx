//! v0.3 KAT — close-payload codec + control-frame
//! constraint enforcement via the public entry points.
//!
//! Close-payload tests round-trip `(Option<CloseCode>,
//! reason)` pairs through `encode_close_payload` /
//! `decode_close_payload`. Control-frame tests construct
//! malformed frames at the byte level and assert
//! `decode_frame` rejects them.

use grpcx_ws::{
    CloseCode, Frame, Opcode, WsError, decode_close_payload, decode_frame, encode_close_payload,
    encode_frame,
};

// ============================================================
// Close-payload encode/decode round-trip.
// ============================================================

#[test]
fn close_payload_empty_round_trip() {
    let bytes = encode_close_payload(None, "");
    assert!(bytes.is_empty());
    let (code, reason) = decode_close_payload(&bytes).unwrap();
    assert!(code.is_none());
    assert!(reason.is_empty());
}

#[test]
fn close_payload_code_only_round_trip() {
    let bytes = encode_close_payload(Some(CloseCode::Normal), "");
    // 1000 BE = 0x03 0xE8.
    assert_eq!(bytes, [0x03, 0xE8]);
    let (code, reason) = decode_close_payload(&bytes).unwrap();
    assert_eq!(code, Some(CloseCode::Normal));
    assert!(reason.is_empty());
}

#[test]
fn close_payload_ascii_reason_round_trip() {
    let bytes = encode_close_payload(Some(CloseCode::GoingAway), "bye");
    // 1001 BE + "bye".
    assert_eq!(bytes, [0x03, 0xE9, b'b', b'y', b'e']);
    let (code, reason) = decode_close_payload(&bytes).unwrap();
    assert_eq!(code, Some(CloseCode::GoingAway));
    assert_eq!(reason, "bye");
}

#[test]
fn close_payload_unicode_reason_round_trip() {
    let bytes = encode_close_payload(Some(CloseCode::PolicyViolation), "héllo🦀");
    let (code, reason) = decode_close_payload(&bytes).unwrap();
    assert_eq!(code, Some(CloseCode::PolicyViolation));
    assert_eq!(reason, "héllo🦀");
}

#[test]
fn close_payload_custom_code_round_trip() {
    let bytes = encode_close_payload(Some(CloseCode::Custom(4042)), "");
    // 4042 BE = 0x0F 0xCA.
    assert_eq!(bytes, [0x0F, 0xCA]);
    let (code, reason) = decode_close_payload(&bytes).unwrap();
    assert_eq!(code, Some(CloseCode::Custom(4042)));
    assert!(reason.is_empty());
}

#[test]
fn close_payload_code_none_drops_reason() {
    // Documented behaviour: `code = None` always produces
    // an empty payload; `reason` is ignored because the
    // wire has no slot for it without a code.
    let bytes = encode_close_payload(None, "ignored");
    assert!(bytes.is_empty());
}

// ============================================================
// Close-payload reject paths.
// ============================================================

#[test]
fn close_payload_one_byte_rejected() {
    let err = decode_close_payload(&[0x03]).unwrap_err();
    assert_eq!(err, WsError::Truncated);
}

#[test]
fn close_payload_reserved_code_rejected() {
    // 1004 BE = 0x03 0xEC.
    let err = decode_close_payload(&[0x03, 0xEC]).unwrap_err();
    assert_eq!(err, WsError::InvalidCloseCode(1004));
}

#[test]
fn close_payload_non_utf8_reason_rejected() {
    // 1000 BE + non-UTF-8 bytes.
    let bytes = [0x03, 0xE8, 0xFF, 0xFE];
    let err = decode_close_payload(&bytes).unwrap_err();
    assert_eq!(err, WsError::InvalidUtf8);
}

// ============================================================
// Control-frame constraint enforcement at decode.
// ============================================================

#[test]
fn control_frame_fin_zero_rejected() {
    // Ping (0x9) with FIN=0 → byte 0 = 0x09.
    let wire = [0x09, 0x00];
    let err = decode_frame(&wire).unwrap_err();
    assert_eq!(err, WsError::FragmentedControlFrame);
}

#[test]
fn control_frame_close_fin_zero_rejected() {
    // Close (0x8) with FIN=0.
    let wire = [0x08, 0x00];
    let err = decode_frame(&wire).unwrap_err();
    assert_eq!(err, WsError::FragmentedControlFrame);
}

#[test]
fn control_frame_oversize_rejected_at_126() {
    // Ping with FIN=1 + length encoded as 126 (extended).
    // Length-126 + value=126 → 126 bytes payload — should
    // reject because control payloads must be ≤ 125.
    let mut wire: Vec<u8> = vec![0x89, 0x7E, 0x00, 0x7E];
    wire.extend_from_slice(&[0xAA; 126]);
    let err = decode_frame(&wire).unwrap_err();
    assert_eq!(err, WsError::PayloadTooLarge);
}

#[test]
fn control_frame_boundary_125_accepted() {
    // Ping with FIN=1 + length=125 (short form). At
    // the boundary; should accept.
    let f = Frame {
        fin: true,
        opcode: Opcode::Ping,
        payload: vec![0xBB; 125],
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&f, &mut wire);
    let (decoded, _) = decode_frame(&wire).unwrap();
    assert_eq!(decoded, f);
}

// ============================================================
// Data-frame constraints unchanged — FIN=0 + Text is the
// fragmented-message case and stays legal.
// ============================================================

#[test]
fn data_frame_fragmented_still_accepted() {
    // Text with FIN=0 (first fragment of a fragmented
    // message); payload ≤ 125 short-form. Should accept;
    // the FIN=0 control-frame rule only applies to
    // control opcodes.
    let wire = [0x01, 0x03, b'h', b'i', b'!'];
    let (decoded, _) = decode_frame(&wire).unwrap();
    assert!(!decoded.fin);
    assert_eq!(decoded.opcode, Opcode::Text);
    assert_eq!(decoded.payload, b"hi!");
}

// ============================================================
// is_control helper smoke.
// ============================================================

#[test]
fn opcode_is_control_classification() {
    assert!(Opcode::Close.is_control());
    assert!(Opcode::Ping.is_control());
    assert!(Opcode::Pong.is_control());
    assert!(!Opcode::Continuation.is_control());
    assert!(!Opcode::Text.is_control());
    assert!(!Opcode::Binary.is_control());
}
