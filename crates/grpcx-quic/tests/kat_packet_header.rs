//! Packet header encode + decode round-trip per RFC 9000 §17.
//!
//! v0.2's KAT scope: build a header value, encode it, decode the
//! bytes back, and require structural equality. The Long-header
//! types covered are Initial + Handshake (0-RTT shares Initial's
//! framing minus the token; Retry is deferred to v0.4).

use grpcx_quic::{
    ConnectionId, LongHeader, LongHeaderType, PacketHeader, ShortHeader, decode_header,
    encode_header,
};

fn cid(bytes: &[u8]) -> ConnectionId {
    assert!(bytes.len() <= 20);
    let mut out = ConnectionId {
        bytes: [0u8; 20],
        len: bytes.len() as u8,
    };
    out.bytes[..bytes.len()].copy_from_slice(bytes);
    out
}

#[test]
fn long_initial_round_trip() {
    let header = LongHeader {
        type_: LongHeaderType::Initial,
        version: 0x0000_0001,
        dcid: cid(&[
            0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            0x00, 0x11, 0x22,
        ]),
        scid: cid(&[]),
        token: Some(vec![0xde, 0xad, 0xbe, 0xef]),
        payload_len: 1162,
        packet_number: 0x0000_0001,
        pn_len: 4,
    };
    let original = PacketHeader::Long(header.clone());

    let mut wire = Vec::new();
    let n = encode_header(&original, &mut wire).expect("encode");
    assert_eq!(n, wire.len());

    let (decoded, consumed) = decode_header(&wire, 0).expect("decode");
    assert_eq!(consumed, wire.len());
    assert_eq!(decoded, original);
}

#[test]
fn long_handshake_round_trip() {
    let header = LongHeader {
        type_: LongHeaderType::Handshake,
        version: 0x0000_0001,
        dcid: cid(&[]),
        scid: cid(&[0xab; 8]),
        token: None,
        payload_len: 256,
        packet_number: 0x42,
        pn_len: 1,
    };
    let original = PacketHeader::Long(header);

    let mut wire = Vec::new();
    encode_header(&original, &mut wire).expect("encode");
    let (decoded, _) = decode_header(&wire, 0).expect("decode");
    assert_eq!(decoded, original);
}

#[test]
fn long_zero_rtt_round_trip() {
    let header = LongHeader {
        type_: LongHeaderType::ZeroRtt,
        version: 0x0000_0001,
        dcid: cid(&[1, 2, 3, 4]),
        scid: cid(&[5, 6, 7, 8]),
        token: None,
        payload_len: 128,
        packet_number: 0x0102,
        pn_len: 2,
    };
    let original = PacketHeader::Long(header);

    let mut wire = Vec::new();
    encode_header(&original, &mut wire).expect("encode");
    let (decoded, _) = decode_header(&wire, 0).expect("decode");
    assert_eq!(decoded, original);
}

#[test]
fn short_one_rtt_round_trip() {
    let header = ShortHeader {
        dcid: cid(&[9, 8, 7, 6, 5, 4, 3, 2]),
        packet_number: 0x0010_0000,
        pn_len: 3,
        key_phase: true,
        spin: false,
    };
    let original = PacketHeader::Short(header);

    let mut wire = Vec::new();
    encode_header(&original, &mut wire).expect("encode");
    // Short header receiver knows the dcid length per-connection;
    // pass it explicitly here.
    let (decoded, _) = decode_header(&wire, 8).expect("decode");
    assert_eq!(decoded, original);
}

#[test]
fn short_spin_bit_round_trip() {
    let header = ShortHeader {
        dcid: cid(&[0xaa, 0xbb, 0xcc, 0xdd]),
        packet_number: 0x7f,
        pn_len: 1,
        key_phase: false,
        spin: true,
    };
    let original = PacketHeader::Short(header);

    let mut wire = Vec::new();
    encode_header(&original, &mut wire).expect("encode");
    let (decoded, _) = decode_header(&wire, 4).expect("decode");
    assert_eq!(decoded, original);
}

#[test]
fn truncated_long_header_rejected() {
    let header = PacketHeader::Long(LongHeader {
        type_: LongHeaderType::Initial,
        version: 0x0000_0001,
        dcid: cid(&[0; 8]),
        scid: cid(&[]),
        token: Some(vec![]),
        payload_len: 128,
        packet_number: 0,
        pn_len: 1,
    });
    let mut wire = Vec::new();
    encode_header(&header, &mut wire).expect("encode");

    // Drop the last byte; decode should error.
    let trunc = &wire[..wire.len() - 1];
    assert!(decode_header(trunc, 0).is_err());
}

#[test]
fn pn_truncation_round_trip() {
    // PN truncated to 2 bytes; full value 0x1234.
    let header = LongHeader {
        type_: LongHeaderType::Handshake,
        version: 0x0000_0001,
        dcid: cid(&[]),
        scid: cid(&[]),
        token: None,
        payload_len: 32,
        packet_number: 0x1234,
        pn_len: 2,
    };
    let original = PacketHeader::Long(header);
    let mut wire = Vec::new();
    encode_header(&original, &mut wire).expect("encode");
    let (decoded, _) = decode_header(&wire, 0).expect("decode");
    assert_eq!(decoded, original);
}
