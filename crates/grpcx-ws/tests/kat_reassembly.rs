//! v0.4 KAT — message reassembly state machine via the
//! public `Reassembler` entry point.
//!
//! Each case feeds a sequence of frames into a fresh
//! `Reassembler` and asserts on the per-frame
//! `ReassemblyOutput` and the final reassembled `Message`
//! (where applicable).

use grpcx_ws::{Frame, Message, MessageKind, Opcode, Reassembler, ReassemblyOutput, WsError};

fn text(fin: bool, payload: &[u8]) -> Frame {
    Frame {
        fin,
        opcode: Opcode::Text,
        payload: payload.to_vec(),
        ..Frame::default()
    }
}

fn binary(fin: bool, payload: &[u8]) -> Frame {
    Frame {
        fin,
        opcode: Opcode::Binary,
        payload: payload.to_vec(),
        ..Frame::default()
    }
}

fn cont(fin: bool, payload: &[u8]) -> Frame {
    Frame {
        fin,
        opcode: Opcode::Continuation,
        payload: payload.to_vec(),
        ..Frame::default()
    }
}

fn ping(payload: &[u8]) -> Frame {
    Frame {
        fin: true,
        opcode: Opcode::Ping,
        payload: payload.to_vec(),
        ..Frame::default()
    }
}

fn close() -> Frame {
    Frame {
        fin: true,
        opcode: Opcode::Close,
        ..Frame::default()
    }
}

// ============================================================
// Single-frame complete data messages.
// ============================================================

#[test]
fn single_fin_text_emits_message() {
    let mut r = Reassembler::new();
    let out = r.feed(text(true, b"hello")).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: b"hello".to_vec(),
        })
    );
}

#[test]
fn single_fin_binary_emits_message() {
    let mut r = Reassembler::new();
    let out = r.feed(binary(true, &[0xDE, 0xAD, 0xBE, 0xEF])).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Binary,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        })
    );
}

// ============================================================
// Fragmentation — 2 and 3 frame sequences.
// ============================================================

#[test]
fn two_frame_text_fragmentation() {
    let mut r = Reassembler::new();
    assert_eq!(
        r.feed(text(false, b"par")).unwrap(),
        ReassemblyOutput::Pending
    );
    let out = r.feed(cont(true, b"t!")).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: b"part!".to_vec(),
        })
    );
}

#[test]
fn three_frame_text_fragmentation() {
    let mut r = Reassembler::new();
    assert_eq!(
        r.feed(text(false, b"foo")).unwrap(),
        ReassemblyOutput::Pending
    );
    assert_eq!(
        r.feed(cont(false, b"bar")).unwrap(),
        ReassemblyOutput::Pending
    );
    let out = r.feed(cont(true, b"baz")).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: b"foobarbaz".to_vec(),
        })
    );
}

#[test]
fn three_frame_binary_fragmentation() {
    let mut r = Reassembler::new();
    r.feed(binary(false, &[1, 2])).unwrap();
    r.feed(cont(false, &[3, 4])).unwrap();
    let out = r.feed(cont(true, &[5, 6])).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Binary,
            payload: vec![1, 2, 3, 4, 5, 6],
        })
    );
}

#[test]
fn empty_payload_fragmentation() {
    // FIN=0 empty + FIN=1 empty Continuation → empty Message.
    let mut r = Reassembler::new();
    assert_eq!(r.feed(text(false, &[])).unwrap(), ReassemblyOutput::Pending);
    let out = r.feed(cont(true, &[])).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: Vec::new(),
        })
    );
}

// ============================================================
// Control frames interleave per RFC 6455 § 5.5 — state
// preserved across the control frame.
// ============================================================

#[test]
fn control_ping_interleaves_between_fragments() {
    let mut r = Reassembler::new();
    assert_eq!(
        r.feed(text(false, b"start")).unwrap(),
        ReassemblyOutput::Pending
    );
    // A Ping arrives mid-fragmentation; should pass through
    // unchanged and not touch the buffer.
    let ping_payload = b"keepalive".to_vec();
    let ping_frame = ping(&ping_payload);
    let out = r.feed(ping_frame.clone()).unwrap();
    assert_eq!(out, ReassemblyOutput::Control(ping_frame));
    // Next continuation completes the original Text message.
    let out = r.feed(cont(true, b"-end")).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: b"start-end".to_vec(),
        })
    );
}

#[test]
fn control_close_at_empty_state_passes_through() {
    let mut r = Reassembler::new();
    let close_frame = close();
    let out = r.feed(close_frame.clone()).unwrap();
    assert_eq!(out, ReassemblyOutput::Control(close_frame));
}

// ============================================================
// Protocol-error paths.
// ============================================================

#[test]
fn continuation_at_empty_errors() {
    let mut r = Reassembler::new();
    let err = r.feed(cont(true, b"orphan")).unwrap_err();
    assert!(matches!(err, WsError::ProtocolError(msg) if msg.contains("continuation")));
}

#[test]
fn new_text_mid_fragmentation_errors() {
    let mut r = Reassembler::new();
    r.feed(text(false, b"part-a")).unwrap();
    let err = r.feed(text(true, b"second-msg")).unwrap_err();
    assert!(matches!(err, WsError::ProtocolError(msg) if msg.contains("previous message")));
}

#[test]
fn new_binary_mid_fragmentation_errors() {
    let mut r = Reassembler::new();
    r.feed(text(false, b"part-a")).unwrap();
    let err = r.feed(binary(true, &[0xAA])).unwrap_err();
    assert!(matches!(err, WsError::ProtocolError(_)));
}

// ============================================================
// UTF-8 validation on emitted Text payloads.
// ============================================================

#[test]
fn non_utf8_single_text_rejected() {
    let mut r = Reassembler::new();
    let err = r.feed(text(true, &[0xFF, 0xFE])).unwrap_err();
    assert_eq!(err, WsError::InvalidUtf8);
}

#[test]
fn fragment_boundary_splits_multibyte_utf8() {
    // 🦀 is U+1F980 → 4-byte UTF-8 sequence F0 9F A6 80.
    // Split at the fragment boundary: 2 bytes in opener, 2
    // bytes in continuation. Should reassemble + validate
    // cleanly — per-fragment validation would falsely
    // reject. Whole-message validation succeeds.
    let mut r = Reassembler::new();
    r.feed(text(false, &[0xF0, 0x9F])).unwrap();
    let out = r.feed(cont(true, &[0xA6, 0x80])).unwrap();
    assert_eq!(
        out,
        ReassemblyOutput::Message(Message {
            kind: MessageKind::Text,
            payload: "🦀".as_bytes().to_vec(),
        })
    );
}

#[test]
fn non_utf8_reassembled_text_rejected() {
    let mut r = Reassembler::new();
    r.feed(text(false, b"ok-")).unwrap();
    let err = r.feed(cont(true, &[0xFF])).unwrap_err();
    assert_eq!(err, WsError::InvalidUtf8);
}

// ============================================================
// Default / new equivalence smoke.
// ============================================================

#[test]
fn default_state_accepts_first_frame() {
    let mut r: Reassembler = Default::default();
    let out = r.feed(text(true, b"first")).unwrap();
    assert!(matches!(out, ReassemblyOutput::Message(_)));
}

// ============================================================
// Sequential messages on the same reassembler.
// ============================================================

#[test]
fn two_sequential_messages_on_same_reassembler() {
    let mut r = Reassembler::new();
    // First message — single Text frame.
    let m1 = r.feed(text(true, b"hello")).unwrap();
    assert!(matches!(m1, ReassemblyOutput::Message(_)));
    // Second message — single Binary frame; state was
    // reset to Empty after the first message emitted.
    let m2 = r.feed(binary(true, b"world")).unwrap();
    assert!(matches!(m2, ReassemblyOutput::Message(_)));
}
