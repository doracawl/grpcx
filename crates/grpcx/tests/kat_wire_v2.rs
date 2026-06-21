//! Wire-format v2 KAT — known-answer test vectors from
//! `_dev/SPEC-wire-v2.md` § 9.
//!
//! Every test pair drives the codec in two directions:
//! 1. Decode the SPEC byte vector into a `Header` (and
//!    metadata block / payload when present), assert
//!    every field matches.
//! 2. Re-encode the structured form and assert the bytes
//!    match the SPEC vector exactly.
//!
//! Conformance to v2 means both directions succeed for
//! every vector in this file.
//!
//! T5.1 makes `_dev/kat-vectors/9.x-*.hex` the single
//! source of truth — these tests load the SPEC byte
//! sequences from those files (via [`load_kat`]) rather
//! than inlining them. The TS / C++ KAT test
//! (`ts/packages/grpcx-native/test/kat_wire_v2.test.mjs`)
//! loads the same files and asserts the same round-trip
//! identity. Both codecs validate against the files,
//! not against each other.

use std::path::PathBuf;

use grpcx::wire_v2::{
    Flags, FrameType, HEADER_LEN, Header, MetadataEntry, block_wire_len, decode_header,
    decode_header_from_slice, decode_metadata_block, encode_header, encode_metadata_block,
    entry_wire_len,
};

/// Load a KAT vector from `_dev/kat-vectors/{name}.hex`.
/// `#` starts an end-of-line comment; whitespace
/// (including newlines) is ignored; every other token is
/// a 2-hex-digit byte read left-to-right, top-to-bottom.
fn load_kat(name: &str) -> Vec<u8> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/grpcx → workspace root → _dev/kat-vectors
    path.push("..");
    path.push("..");
    path.push("_dev");
    path.push("kat-vectors");
    path.push(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()));
    let mut bytes = Vec::new();
    for line in text.lines() {
        let no_comment = line.split('#').next().unwrap_or("");
        for tok in no_comment.split_ascii_whitespace() {
            assert_eq!(tok.len(), 2, "hex byte must be 2 chars, got {tok:?}");
            let b =
                u8::from_str_radix(tok, 16).unwrap_or_else(|e| panic!("bad hex byte {tok:?}: {e}"));
            bytes.push(b);
        }
    }
    bytes
}

// ============================================================
// § 9.1 — Minimal unary BEGIN with inline body
// ============================================================

/// SPEC § 9.1 — stream 1, service "demo.v1.Greeter",
/// method "SayHello", kind Unary (0), body "Hi!".
///
/// Total: 16 + 65 = 81 bytes. Loaded from
/// `_dev/kat-vectors/9.1-unary-begin.hex`.
fn kat_9_1() -> Vec<u8> {
    load_kat("9.1-unary-begin.hex")
}

#[test]
fn kat_9_1_decode() {
    let kat_9_1 = kat_9_1();
    assert_eq!(kat_9_1.len(), 81);
    let header = decode_header_from_slice(&kat_9_1[..HEADER_LEN]).expect("header decodes");
    assert_eq!(header.stream_id, 1);
    assert_eq!(header.frame_type, FrameType::Begin);
    assert!(header.flags.contains(Flags::HAS_METADATA));
    assert!(header.flags.contains(Flags::END_STREAM));
    assert!(!header.flags.contains(Flags::CONTINUATION));
    assert_eq!(header.metadata_count, 3);
    assert_eq!(header.payload_len, 65);

    let payload = &kat_9_1[HEADER_LEN..];
    assert_eq!(payload.len(), header.payload_len as usize);

    let (entries, rest) =
        decode_metadata_block(payload, header.metadata_count).expect("metadata decodes");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, b":service");
    assert_eq!(entries[0].value, b"demo.v1.Greeter");
    assert_eq!(entries[1].name, b":method");
    assert_eq!(entries[1].value, b"SayHello");
    assert_eq!(entries[2].name, b":kind");
    assert_eq!(entries[2].value, &[0u8][..]);
    assert_eq!(rest, b"Hi!");
}

#[test]
fn kat_9_1_encode() {
    let header = Header {
        stream_id: 1,
        frame_type: FrameType::Begin,
        flags: Flags::HAS_METADATA.insert(Flags::END_STREAM),
        metadata_count: 3,
        payload_len: 65,
    };
    let entries = [
        MetadataEntry {
            name: b":service",
            value: b"demo.v1.Greeter",
        },
        MetadataEntry {
            name: b":method",
            value: b"SayHello",
        },
        MetadataEntry {
            name: b":kind",
            value: &[0u8],
        },
    ];
    let body: &[u8] = b"Hi!";

    let mut wire = vec![0u8; HEADER_LEN + header.payload_len as usize];
    let header_buf: &mut [u8; HEADER_LEN] = (&mut wire[..HEADER_LEN]).try_into().unwrap();
    encode_header(&header, header_buf);

    let meta_len = block_wire_len(&entries);
    assert_eq!(meta_len, 62);
    let written = encode_metadata_block(&entries, &mut wire[HEADER_LEN..HEADER_LEN + meta_len]);
    assert_eq!(written, meta_len);
    wire[HEADER_LEN + meta_len..].copy_from_slice(body);

    assert_eq!(wire, kat_9_1());
}

// ============================================================
// § 9.2 — Minimal END with no trailers
// ============================================================

#[test]
fn kat_9_2_round_trip() {
    let kat_9_2 = load_kat("9.2-end-no-trailers.hex");
    assert_eq!(kat_9_2.len(), 16);

    // decode
    let header = decode_header_from_slice(&kat_9_2).expect("9.2 header decodes");
    assert_eq!(header.stream_id, 1);
    assert_eq!(header.frame_type, FrameType::End);
    assert!(header.flags.contains(Flags::END_STREAM));
    assert!(!header.flags.contains(Flags::HAS_METADATA));
    assert_eq!(header.metadata_count, 0);
    assert_eq!(header.payload_len, 0);

    // encode
    let mut out = [0u8; HEADER_LEN];
    encode_header(&header, &mut out);
    assert_eq!(&out[..], &kat_9_2[..]);
}

// ============================================================
// § 9.3 — RESET with Cancelled
// ============================================================

#[test]
fn kat_9_3_round_trip() {
    let kat_9_3 = load_kat("9.3-reset-cancelled.hex");
    assert_eq!(kat_9_3.len(), 35);

    let header = decode_header_from_slice(&kat_9_3[..HEADER_LEN]).expect("9.3 header decodes");
    assert_eq!(header.stream_id, 5);
    assert_eq!(header.frame_type, FrameType::Reset);
    assert!(header.flags.contains(Flags::END_STREAM));
    assert!(!header.flags.contains(Flags::HAS_METADATA));
    assert_eq!(header.metadata_count, 0);
    assert_eq!(header.payload_len, 19);

    // RESET payload: status_code u32 + msg_len u32 + msg bytes
    let payload = &kat_9_3[HEADER_LEN..];
    let status_code = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let msg_len = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    assert_eq!(status_code, 1); // Cancelled
    assert_eq!(msg_len, 11);
    assert_eq!(&payload[8..8 + msg_len as usize], b"client gone");

    // encode header round-trips
    let mut out = [0u8; HEADER_LEN];
    encode_header(&header, &mut out);
    assert_eq!(&out[..], &kat_9_3[..HEADER_LEN]);
}

// ============================================================
// § 9.4 — PING with opaque payload
// ============================================================

#[test]
fn kat_9_4_round_trip() {
    let kat_9_4 = load_kat("9.4-ping.hex");
    assert_eq!(kat_9_4.len(), 20);

    let header = decode_header_from_slice(&kat_9_4[..HEADER_LEN]).expect("9.4 header decodes");
    assert_eq!(header.stream_id, 0);
    assert_eq!(header.frame_type, FrameType::Ping);
    assert_eq!(header.flags.bits(), 0);
    assert_eq!(header.metadata_count, 0);
    assert_eq!(header.payload_len, 4);
    assert_eq!(&kat_9_4[HEADER_LEN..], &[0x42, 0x13, 0x37, 0x00]);

    let mut out = [0u8; HEADER_LEN];
    encode_header(&header, &mut out);
    assert_eq!(&out[..], &kat_9_4[..HEADER_LEN]);
}

// ============================================================
// Negative tests — receivers MUST reject these
// ============================================================

#[test]
fn rejects_unknown_frame_type() {
    let mut bytes = [0u8; HEADER_LEN];
    bytes[4] = 0xff; // not 0x01..=0x06
    let err = decode_header_from_slice(&bytes).expect_err("unknown frame type rejected");
    match err {
        grpcx::wire_v2::WireError::UnknownFrameType(b) => assert_eq!(b, 0xff),
        other => panic!("wrong error variant: {other:?}"),
    }
}

#[test]
fn rejects_short_buffer() {
    let short = [0u8; HEADER_LEN - 1];
    let err = decode_header_from_slice(&short).expect_err("short buffer rejected");
    match err {
        grpcx::wire_v2::WireError::BufferTooShort { need, have } => {
            assert_eq!(need, HEADER_LEN);
            assert_eq!(have, HEADER_LEN - 1);
        }
        other => panic!("wrong error variant: {other:?}"),
    }
}

#[test]
fn rejects_zero_name_len() {
    // One synthetic entry with name_len = 0.
    let payload = [
        0x00, 0x00, // name_len = 0
        0x00, 0x00, 0x00, 0x00, // value_len = 0
    ];
    let err = decode_metadata_block(&payload, 1).expect_err("zero name_len rejected");
    match err {
        grpcx::wire_v2::WireError::InvalidMetadataNameLen(0) => {}
        other => panic!("wrong error variant: {other:?}"),
    }
}

#[test]
fn rejects_oversized_name_len() {
    // name_len = 1025 — one above NAME_LEN_MAX.
    let payload = [
        0x01, 0x04, // name_len = 0x0401 = 1025
        0x00, 0x00, 0x00, 0x00, // value_len = 0
    ];
    let err = decode_metadata_block(&payload, 1).expect_err("oversized name_len rejected");
    match err {
        grpcx::wire_v2::WireError::InvalidMetadataNameLen(1025) => {}
        other => panic!("wrong error variant: {other:?}"),
    }
}

#[test]
fn rejects_truncated_metadata() {
    // name_len = 8 + value_len = 4 = 12 bytes claimed,
    // but only 6 entry-header bytes provided.
    let payload = [
        0x08, 0x00, // name_len = 8
        0x04, 0x00, 0x00, 0x00, // value_len = 4
    ];
    let err = decode_metadata_block(&payload, 1).expect_err("truncated metadata rejected");
    assert_eq!(err, grpcx::wire_v2::WireError::TruncatedMetadata);
}

// ============================================================
// Sanity — header round-trip on a comprehensive value
// ============================================================

#[test]
fn header_round_trip_all_frame_types() {
    for &ft in &[
        FrameType::Begin,
        FrameType::Data,
        FrameType::End,
        FrameType::Reset,
        FrameType::Ping,
        FrameType::Pong,
    ] {
        let h = Header {
            stream_id: 0xdead_beef,
            frame_type: ft,
            flags: Flags::END_STREAM
                .insert(Flags::HAS_METADATA)
                .insert(Flags::CONTINUATION),
            metadata_count: 5,
            payload_len: 1_048_576,
        };
        let mut buf = [0u8; HEADER_LEN];
        encode_header(&h, &mut buf);
        let h2 = decode_header(&buf).expect("round-trips");
        assert_eq!(h, h2);
        // reserved bytes MUST be zero on send.
        assert_eq!(&buf[12..16], &[0, 0, 0, 0]);
    }
}

#[test]
fn entry_wire_len_matches_block() {
    let entries = [
        MetadataEntry {
            name: b"a",
            value: b"",
        },
        MetadataEntry {
            name: b"hello",
            value: b"world",
        },
    ];
    let sum: usize = entries
        .iter()
        .map(|e| entry_wire_len(e.name, e.value))
        .sum();
    assert_eq!(sum, block_wire_len(&entries));
    // 6 (entry header) + 1 (name "a") + 0 (empty value)
    // + 6 (entry header) + 5 (name "hello") + 5 (value "world")
    assert_eq!(sum, 7 + 16);
}
