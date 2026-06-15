//! L9 minimum-shape example: drive every major piece of the
//! `grpcx-ws` public surface in a single end-to-end run.
//!
//! Used by:
//! - `BUDGETS.md` BINSIZE measurement: `cargo bloat --release
//!   --example min_ws --crates`.
//! - `cargo run --example min_ws --release` smoke-test as part
//!   of the v0.7 / v9.0 gate.

use grpcx_ws::{
    Frame, Handshake, Message, MessageKind, Opcode, PermessageDeflateOffer, Reassembler,
    ReassemblyOutput, WsError, build_server_handshake_response, decode_frame, derive_accept_key,
    encode_frame, parse_client_handshake_request, parse_extension_header,
    perform_client_handshake_request,
};

fn main() {
    // ----- Handshake round-trip -------------------------------
    // 1. Client builds its upgrade request with a known nonce
    //    (RFC 6455 § 1.3 reference vector).
    let client_hs = Handshake {
        request_key: "dGhlIHNhbXBsZSBub25jZQ==".to_string(),
        ..Default::default()
    };
    let client_req = perform_client_handshake_request("server.example.com", "/chat", &client_hs);

    // 2. Server parses the request and derives the accept key.
    let parsed =
        parse_client_handshake_request(client_req.as_bytes()).expect("client request parses");
    assert_eq!(parsed.request_key, "dGhlIHNhbXBsZSBub25jZQ==");

    let server_hs = Handshake {
        accept_key: derive_accept_key(&parsed.request_key),
        ..Default::default()
    };
    assert_eq!(server_hs.accept_key, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");

    let _server_resp = build_server_handshake_response(&server_hs);

    // ----- Frame codec round-trip ----------------------------
    // 3. Client sends a masked Text frame "Hello".
    let frame = Frame {
        fin: true,
        opcode: Opcode::Text,
        mask: Some([0x37, 0xFA, 0x21, 0x3D]),
        payload: b"Hello".to_vec(),
        ..Frame::default()
    };
    let mut wire = Vec::new();
    encode_frame(&frame, &mut wire);
    // RFC 6455 § 5.7 reference masked payload: 7F 9F 4D 51 58.
    assert_eq!(
        wire,
        [
            0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, // header + mask key
            0x7F, 0x9F, 0x4D, 0x51, 0x58, // masked "Hello"
        ]
    );
    let (decoded, consumed) = decode_frame(&wire).expect("decode succeeds");
    assert_eq!(consumed, wire.len());
    assert_eq!(decoded.payload, b"Hello");

    // ----- Reassembler — fragmented Text message -------------
    // 4. Feed three Text-Continuation-Continuation frames into
    //    the reassembler; expect a single Text message.
    let mut r = Reassembler::new();
    let f1 = Frame {
        fin: false,
        opcode: Opcode::Text,
        payload: b"foo".to_vec(),
        ..Frame::default()
    };
    let f2 = Frame {
        fin: false,
        opcode: Opcode::Continuation,
        payload: b"bar".to_vec(),
        ..Frame::default()
    };
    let f3 = Frame {
        fin: true,
        opcode: Opcode::Continuation,
        payload: b"baz".to_vec(),
        ..Frame::default()
    };
    assert!(matches!(
        r.feed(f1).expect("first fragment"),
        ReassemblyOutput::Pending
    ));
    assert!(matches!(
        r.feed(f2).expect("middle fragment"),
        ReassemblyOutput::Pending
    ));
    match r.feed(f3).expect("final fragment") {
        ReassemblyOutput::Message(Message { kind, payload }) => {
            assert_eq!(kind, MessageKind::Text);
            assert_eq!(payload, b"foobarbaz");
        }
        other => panic!("expected Message, got {other:?}"),
    }

    // ----- Extension negotiation surface ---------------------
    // 5. Parse a permessage-deflate offer with all parameters.
    let offers = parse_extension_header(
        "permessage-deflate; server_no_context_takeover; \
         client_max_window_bits=15",
    )
    .expect("extension parses");
    let pmd = PermessageDeflateOffer::parse(&offers[0]).expect("PMD parses");
    assert!(pmd.server_no_context_takeover);
    assert_eq!(pmd.client_max_window_bits, Some(15));

    // ----- Error surface smoke -------------------------------
    // 6. Confirm `WsError::FragmentedControlFrame` fires on a
    //    Ping with FIN=0 (RFC 6455 § 5.5 violation).
    let bad = [0x09, 0x00]; // Ping (0x9) with FIN=0.
    assert!(matches!(
        decode_frame(&bad),
        Err(WsError::FragmentedControlFrame)
    ));
}
