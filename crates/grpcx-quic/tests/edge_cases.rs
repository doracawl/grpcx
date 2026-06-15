//! Edge-case coverage for the v0.7 stabilisation gate.
//!
//! These tests focus on rejection paths and rarely-hit branches in
//! the public surface (frame / packet / protection / initial / varint).
//! Happy-path coverage already lives in the `kat_*` and `integ_*`
//! suites; this file targets the remaining lines that would
//! otherwise drag the workspace coverage number below the v0.7
//! gate of 95%.

use grpcx_quic::{
    AeadSuite, ConnectionId, EcnCounts, Error, Frame, LongHeader, LongHeaderType, PacketHeader,
    ShortHeader, Side, StreamId, apply_header_protection, build_initial_packet,
    build_initial_packet_with_initial_dcid, decode_frame, decode_header, encode_frame,
    encode_header, frame_varint_len, parse_initial_packet, protect_payload,
    remove_header_protection, unprotect_payload, varint_field_len,
};

fn cid_of(bytes: &[u8]) -> ConnectionId {
    let mut out = ConnectionId {
        bytes: [0u8; 20],
        len: bytes.len() as u8,
    };
    out.bytes[..bytes.len()].copy_from_slice(bytes);
    out
}

// ============================================================
// Frame codec edge cases.
// ============================================================

#[test]
fn frame_new_connection_id_oversize_cid_decode_rejected() {
    // type=0x18, seq=0, retire=0, cid_len=21 (over the 20 max).
    let mut buf: &[u8] = &[
        0x18, // type
        0x00, // seq
        0x00, // retire
        0x15, // cid_len = 21
    ];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn frame_new_connection_id_oversize_cid_encode_rejected() {
    let f = Frame::NewConnectionId {
        sequence_number: 0,
        retire_prior_to: 0,
        connection_id: vec![0u8; 21],
        stateless_reset_token: [0u8; 16],
    };
    let mut out = Vec::new();
    assert_eq!(encode_frame(&f, &mut out), Err(Error::BadFrame));
}

#[test]
fn frame_datagram_no_length_form_round_trip() {
    // Decode-only path: 0x30 (no-length) — encoder always emits 0x31.
    let mut buf: &[u8] = &[0x30, 0xde, 0xad, 0xbe, 0xef];
    let f = decode_frame(&mut buf).unwrap();
    assert!(buf.is_empty());
    assert_eq!(
        f,
        Frame::Datagram {
            data: vec![0xde, 0xad, 0xbe, 0xef],
        }
    );
}

#[test]
fn frame_datagram_with_length_round_trip() {
    let f = Frame::Datagram {
        data: vec![1, 2, 3, 4, 5],
    };
    let mut out = Vec::new();
    encode_frame(&f, &mut out).unwrap();
    let mut cursor: &[u8] = &out;
    assert_eq!(decode_frame(&mut cursor).unwrap(), f);
    assert!(cursor.is_empty());
}

#[test]
fn frame_unknown_type_rejected() {
    // 0x40 = 2-byte varint form, value 0x00 → unknown frame type 0.
    // 0xff is the 8-byte varint prefix; a too-short buf trips the
    // varint reader first, but we want the unknown-type branch.
    let mut buf: &[u8] = &[0x40, 0x2a]; // value = 42
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn frame_path_challenge_short_buf_rejected() {
    let mut buf: &[u8] = &[0x1a, 0x01, 0x02]; // only 2 bytes of payload, need 8
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn frame_truncated_buf_on_read_u8_rejected() {
    // type=0x18 NewConnectionId; seq=0, retire=0 OK; then EOF before cid_len.
    let mut buf: &[u8] = &[0x18, 0x00, 0x00];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn frame_stream_no_length_form_consumes_rest() {
    // 0x08 = STREAM type, no OFF, no LEN, no FIN. SID varint = 0.
    let mut buf: &[u8] = &[0x08, 0x00, 0xaa, 0xbb, 0xcc];
    let f = decode_frame(&mut buf).unwrap();
    assert!(buf.is_empty());
    match f {
        Frame::Stream {
            stream_id: StreamId(0),
            offset: 0,
            data,
            fin: false,
        } => assert_eq!(data, vec![0xaa, 0xbb, 0xcc]),
        _ => panic!("unexpected frame: {f:?}"),
    }
}

#[test]
fn frame_varint_len_helper() {
    assert_eq!(frame_varint_len(0), 1);
    assert_eq!(frame_varint_len(63), 1);
    assert_eq!(frame_varint_len(64), 2);
    assert_eq!(frame_varint_len(16383), 2);
    assert_eq!(frame_varint_len(16384), 4);
    assert_eq!(frame_varint_len(1 << 30), 8);
}

#[test]
fn frame_ack_oversize_range_count_rejected() {
    // Regression: fuzz frame_ops hit this in ~800 execs. range_count
    // declared as an 8-byte varint with the top bits set (i.e.
    // ~2^58) caused `Vec::with_capacity(range_count)` to abort
    // with OOM. The decoder now bounds it by the remaining buffer.
    let mut buf: &[u8] = &[
        0x03, // ACK with ECN
        0x2f, // largest_acked = 47
        0x00, // ack_delay = 0
        // range_count: 8-byte varint (top 2 bits cleared) → huge
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
        0x00, // first_ack_range = 0 (1-byte varint)
    ];
    assert_eq!(decode_frame(&mut buf), Err(Error::BadFrame));
}

#[test]
fn frame_ack_with_ecn_round_trip() {
    let f = Frame::Ack {
        largest_acked: 100,
        ack_delay: 50,
        first_ack_range: 5,
        ranges: vec![(1, 2), (3, 4)],
        ecn: Some(EcnCounts {
            ect0: 1,
            ect1: 2,
            ce: 3,
        }),
    };
    let mut out = Vec::new();
    encode_frame(&f, &mut out).unwrap();
    let mut cursor: &[u8] = &out;
    assert_eq!(decode_frame(&mut cursor).unwrap(), f);
}

// ============================================================
// Packet header codec edge cases.
// ============================================================

#[test]
fn packet_decode_empty_buf_rejected() {
    assert_eq!(decode_header(&[], 0).err(), Some(Error::BadPacket));
}

#[test]
fn packet_encode_long_oversize_dcid_rejected() {
    let mut bad_dcid = cid_of(&[1, 2, 3]);
    bad_dcid.len = 21;
    let h = PacketHeader::Long(LongHeader {
        type_: LongHeaderType::Initial,
        version: 1,
        dcid: bad_dcid,
        scid: cid_of(&[]),
        token: None,
        payload_len: 0,
        packet_number: 0,
        pn_len: 1,
    });
    let mut out = Vec::new();
    assert_eq!(encode_header(&h, &mut out), Err(Error::BadPacket));
}

#[test]
fn packet_encode_long_bad_pn_len_rejected() {
    let h = PacketHeader::Long(LongHeader {
        type_: LongHeaderType::Initial,
        version: 1,
        dcid: cid_of(&[]),
        scid: cid_of(&[]),
        token: None,
        payload_len: 0,
        packet_number: 0,
        pn_len: 0, // invalid
    });
    let mut out = Vec::new();
    assert_eq!(encode_header(&h, &mut out), Err(Error::BadPacket));
}

#[test]
fn packet_encode_short_oversize_dcid_rejected() {
    let mut bad_dcid = cid_of(&[1]);
    bad_dcid.len = 21;
    let h = PacketHeader::Short(ShortHeader {
        dcid: bad_dcid,
        packet_number: 0,
        pn_len: 1,
        key_phase: false,
        spin: false,
    });
    let mut out = Vec::new();
    assert_eq!(encode_header(&h, &mut out), Err(Error::BadPacket));
}

#[test]
fn packet_encode_short_bad_pn_len_rejected() {
    let h = PacketHeader::Short(ShortHeader {
        dcid: cid_of(&[1]),
        packet_number: 0,
        pn_len: 5, // invalid
        key_phase: false,
        spin: false,
    });
    let mut out = Vec::new();
    assert_eq!(encode_header(&h, &mut out), Err(Error::BadPacket));
}

#[test]
fn packet_decode_long_fixed_bit_zero_rejected() {
    // 0b1000_0000 — long form, fixed bit cleared.
    let buf = [0x80u8, 0, 0, 0, 1, 0, 0, 0, 0, 0];
    assert_eq!(decode_header(&buf, 0).err(), Some(Error::BadPacket));
}

#[test]
fn packet_decode_short_fixed_bit_zero_rejected() {
    // 0b0000_0000 — short form, fixed bit cleared.
    let buf = [0x00u8, 0x11, 0x22, 0x33, 0x44];
    assert_eq!(decode_header(&buf, 1).err(), Some(Error::BadPacket));
}

#[test]
fn packet_decode_short_oversize_negotiated_dcid_rejected() {
    let buf = [0x40u8, 0x11];
    // dcid_len > 20 is rejected before we even look at bytes.
    assert_eq!(decode_header(&buf, 21).err(), Some(Error::BadPacket));
}

#[test]
fn packet_decode_long_oversize_dcid_len_rejected() {
    // 1100 0000 + version + dcid_len=21
    let buf = [0xc0u8, 0, 0, 0, 1, 21];
    assert_eq!(decode_header(&buf, 0).err(), Some(Error::BadPacket));
}

#[test]
fn packet_decode_long_oversize_scid_len_rejected() {
    // 1100 0000 + version + dcid_len=0 + scid_len=21
    let buf = [0xc0u8, 0, 0, 0, 1, 0, 21];
    assert_eq!(decode_header(&buf, 0).err(), Some(Error::BadPacket));
}

#[test]
fn packet_long_retry_round_trip() {
    let h = PacketHeader::Long(LongHeader {
        type_: LongHeaderType::Retry,
        version: 1,
        dcid: cid_of(&[1, 2, 3]),
        scid: cid_of(&[4, 5, 6]),
        token: None,
        payload_len: 0,
        packet_number: 0,
        pn_len: 1,
    });
    let mut out = Vec::new();
    encode_header(&h, &mut out).unwrap();
    // Push one filler byte so the truncated-PN read has bytes.
    out.push(0);
    let (h2, _) = decode_header(&out, 0).unwrap();
    if let (PacketHeader::Long(a), PacketHeader::Long(b)) = (&h, &h2) {
        assert_eq!(a.type_, b.type_);
        assert_eq!(b.type_, LongHeaderType::Retry);
    } else {
        panic!("expected Long/Long");
    }
}

#[test]
fn packet_varint_field_len_helper() {
    assert_eq!(varint_field_len(0), 1);
    assert_eq!(varint_field_len(63), 1);
    assert_eq!(varint_field_len(16383), 2);
    assert_eq!(varint_field_len(16384), 4);
    assert_eq!(varint_field_len((1 << 30) - 1), 4);
    assert_eq!(varint_field_len(1 << 30), 8);
}

// ============================================================
// Protection edge cases.
// ============================================================

#[test]
fn protection_aes256_hp_suite_mapping() {
    // Just trip the AES-256 arm of `AeadSuite::hp_suite`.
    let _ = AeadSuite::Aes256Gcm.hp_suite();
    assert_eq!(AeadSuite::Aes128Gcm.tag_len(), 16);
    assert_eq!(AeadSuite::Aes256Gcm.tag_len(), 16);
    assert_eq!(AeadSuite::ChaCha20Poly1305.tag_len(), 16);
}

#[test]
fn protection_iv_wrong_length_rejected() {
    let key = [0u8; 16];
    let iv = [0u8; 11]; // wrong; QUIC nonce is 12
    let aad = [0u8; 1];
    let mut buf = vec![0u8; 32];
    assert_eq!(
        protect_payload(AeadSuite::Aes128Gcm, &key, &iv, 0, &aad, &mut buf, 0),
        Err(Error::BadCrypto)
    );
    assert_eq!(
        unprotect_payload(AeadSuite::Aes128Gcm, &key, &iv, 0, &aad, &mut buf),
        Err(Error::BadCrypto)
    );
}

#[test]
fn protection_apply_bad_pn_len_rejected() {
    let mut pkt = vec![0u8; 32];
    let hp_key = [0u8; 16];
    assert_eq!(
        apply_header_protection(AeadSuite::Aes128Gcm, &hp_key, &mut pkt, 0, 0, true),
        Err(Error::BadCrypto)
    );
    assert_eq!(
        apply_header_protection(AeadSuite::Aes128Gcm, &hp_key, &mut pkt, 0, 5, true),
        Err(Error::BadCrypto)
    );
}

#[test]
fn protection_apply_truncated_pn_slot_rejected() {
    let mut pkt = vec![0u8; 30];
    let hp_key = [0u8; 16];
    // pn_offset + pn_len > packet.len(): pn_offset = 28, pn_len = 4 → 32 > 30.
    // sample-len check fires first; just confirm we don't panic and we report BadCrypto.
    assert_eq!(
        apply_header_protection(AeadSuite::Aes128Gcm, &hp_key, &mut pkt, 28, 4, true),
        Err(Error::BadCrypto)
    );
}

#[test]
fn protection_remove_truncated_sample_rejected() {
    let mut pkt = vec![0u8; 5];
    let hp_key = [0u8; 16];
    assert_eq!(
        remove_header_protection(AeadSuite::Aes128Gcm, &hp_key, &mut pkt, 0, true),
        Err(Error::BadCrypto)
    );
}

// ============================================================
// Initial-packet helper edge cases.
// ============================================================

#[test]
fn initial_parse_short_buf_rejected() {
    let mut buf = [0u8; 5]; // too short
    let dcid = [0u8; 8];
    assert!(parse_initial_packet(&mut buf, &dcid, Side::Server).is_err());
}

#[test]
fn initial_build_with_initial_dcid_separate_from_packet_dcid() {
    // The whole point of the `_with_initial_dcid` variant: the
    // packet's on-wire DCID is *different* from the seed used to
    // derive Initial keys (server → client case).
    let on_wire_dcid = cid_of(&[1, 2, 3]);
    let scid = cid_of(&[4, 5, 6]);
    let seed = [0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8];
    let pkt = build_initial_packet_with_initial_dcid(
        &on_wire_dcid,
        &scid,
        &[],
        0,
        b"hi",
        Side::Server,
        0,
        &seed,
    )
    .unwrap();
    // Parse with the *correct* seed — must succeed.
    let mut p = pkt.clone();
    let (long, frames) = parse_initial_packet(&mut p, &seed, Side::Server).unwrap();
    assert_eq!(long.type_, LongHeaderType::Initial);
    assert!(matches!(&frames[0], Frame::Crypto { data, .. } if data == b"hi"));
}

#[test]
fn initial_build_round_trip_client() {
    let dcid = cid_of(&[0; 8]);
    let scid = cid_of(&[1; 8]);
    let pkt =
        build_initial_packet(&dcid, &scid, &[], 0, b"ClientHello", Side::Client, 1200).unwrap();
    assert!(pkt.len() >= 1200);
    let mut p = pkt.clone();
    let (long, _) =
        parse_initial_packet(&mut p, &dcid.bytes[..dcid.len as usize], Side::Client).unwrap();
    assert_eq!(long.type_, LongHeaderType::Initial);
}
