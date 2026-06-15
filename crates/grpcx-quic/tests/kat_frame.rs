//! QUIC frame codec round-trip per RFC 9000 §19 + RFC 9221.
//!
//! Every variant in [`Frame`] gets a build → encode → decode →
//! eq round trip. STREAM gets 3 variants covering the FIN/LEN/OFF
//! flag combinations the encoder uses; ACK gets two (with + without
//! ECN); DATAGRAM tests both wire forms on the decode side.

use grpcx_quic::{EcnCounts, Frame, StreamId, decode_frame, encode_frame};

fn round_trip(frame: Frame) {
    let mut wire = Vec::new();
    let n = encode_frame(&frame, &mut wire).expect("encode");
    assert_eq!(n, wire.len(), "encode reported wrong length");
    let mut cursor: &[u8] = &wire;
    let decoded = decode_frame(&mut cursor).expect("decode");
    assert_eq!(decoded, frame, "round-trip mismatch");
    assert!(cursor.is_empty(), "decode left trailing bytes");
}

#[test]
fn padding() {
    round_trip(Frame::Padding);
}

#[test]
fn ping() {
    round_trip(Frame::Ping);
}

#[test]
fn ack_no_ecn() {
    round_trip(Frame::Ack {
        largest_acked: 100,
        ack_delay: 25,
        first_ack_range: 5,
        ranges: vec![(2, 3), (4, 1)],
        ecn: None,
    });
}

#[test]
fn ack_with_ecn() {
    round_trip(Frame::Ack {
        largest_acked: 1_000_000,
        ack_delay: 9,
        first_ack_range: 0,
        ranges: vec![],
        ecn: Some(EcnCounts {
            ect0: 42,
            ect1: 7,
            ce: 1,
        }),
    });
}

#[test]
fn reset_stream() {
    round_trip(Frame::ResetStream {
        stream_id: StreamId(1),
        application_error_code: 0x1234,
        final_size: 65536,
    });
}

#[test]
fn stop_sending() {
    round_trip(Frame::StopSending {
        stream_id: StreamId(5),
        application_error_code: 0x42,
    });
}

#[test]
fn crypto() {
    round_trip(Frame::Crypto {
        offset: 0,
        data: vec![0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03],
    });
    round_trip(Frame::Crypto {
        offset: 1024,
        data: vec![],
    });
}

#[test]
fn new_token() {
    round_trip(Frame::NewToken {
        token: b"some-server-issued-token-bytes".to_vec(),
    });
}

#[test]
fn stream_no_offset_with_fin() {
    round_trip(Frame::Stream {
        stream_id: StreamId(4),
        offset: 0,
        data: b"hello, world".to_vec(),
        fin: true,
    });
}

#[test]
fn stream_with_offset_no_fin() {
    round_trip(Frame::Stream {
        stream_id: StreamId(8),
        offset: 4096,
        data: b"continuation".to_vec(),
        fin: false,
    });
}

#[test]
fn stream_empty_with_fin() {
    round_trip(Frame::Stream {
        stream_id: StreamId(12),
        offset: 100,
        data: vec![],
        fin: true,
    });
}

#[test]
fn max_data() {
    round_trip(Frame::MaxData(1 << 24));
}

#[test]
fn max_stream_data() {
    round_trip(Frame::MaxStreamData {
        stream_id: StreamId(16),
        max_data: 1 << 20,
    });
}

#[test]
fn max_streams() {
    round_trip(Frame::MaxStreams {
        bidi: true,
        max: 100,
    });
    round_trip(Frame::MaxStreams {
        bidi: false,
        max: 50,
    });
}

#[test]
fn data_blocked() {
    round_trip(Frame::DataBlocked { data_limit: 4096 });
}

#[test]
fn stream_data_blocked() {
    round_trip(Frame::StreamDataBlocked {
        stream_id: StreamId(20),
        stream_data_limit: 2048,
    });
}

#[test]
fn streams_blocked() {
    round_trip(Frame::StreamsBlocked {
        bidi: true,
        stream_limit: 10,
    });
    round_trip(Frame::StreamsBlocked {
        bidi: false,
        stream_limit: 5,
    });
}

#[test]
fn new_connection_id() {
    round_trip(Frame::NewConnectionId {
        sequence_number: 1,
        retire_prior_to: 0,
        connection_id: vec![0xaa; 8],
        stateless_reset_token: [0x55; 16],
    });
}

#[test]
fn retire_connection_id() {
    round_trip(Frame::RetireConnectionId { sequence_number: 3 });
}

#[test]
fn path_challenge_response() {
    round_trip(Frame::PathChallenge {
        data: [1, 2, 3, 4, 5, 6, 7, 8],
    });
    round_trip(Frame::PathResponse {
        data: [9, 8, 7, 6, 5, 4, 3, 2],
    });
}

#[test]
fn connection_close_transport() {
    round_trip(Frame::ConnectionClose {
        error_code: 0xa,
        frame_type: 0x06, // CRYPTO frame
        is_application: false,
        reason: b"bad handshake".to_vec(),
    });
}

#[test]
fn connection_close_application() {
    round_trip(Frame::ConnectionClose {
        error_code: 0x100,
        frame_type: 0,
        is_application: true,
        reason: b"app-level reason".to_vec(),
    });
}

#[test]
fn handshake_done() {
    round_trip(Frame::HandshakeDone);
}

#[test]
fn datagram_length_prefixed() {
    round_trip(Frame::Datagram {
        data: b"datagram bytes".to_vec(),
    });
}

#[test]
fn datagram_no_length_decode() {
    // Wire form 0x30 (no length) — decoder accepts; encoder always
    // emits 0x31 form (so we hand-craft the bytes here).
    let payload = b"to-end datagram";
    let mut wire = vec![0x30];
    wire.extend_from_slice(payload);
    let mut cursor: &[u8] = &wire;
    let decoded = decode_frame(&mut cursor).expect("decode");
    assert_eq!(
        decoded,
        Frame::Datagram {
            data: payload.to_vec(),
        }
    );
    assert!(cursor.is_empty());
}

#[test]
fn truncated_crypto_rejected() {
    // CRYPTO type byte + offset varint + length=10 but only 5 bytes
    let mut wire = vec![0x06, 0x00, 0x0a];
    wire.extend_from_slice(&[1, 2, 3, 4, 5]);
    let mut cursor: &[u8] = &wire;
    assert!(decode_frame(&mut cursor).is_err());
}

#[test]
fn unknown_frame_type_rejected() {
    let mut cursor: &[u8] = &[0x99];
    assert!(decode_frame(&mut cursor).is_err());
}
