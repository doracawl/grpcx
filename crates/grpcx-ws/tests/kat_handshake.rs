//! v0.5 KAT — HTTP/1.1 upgrade handshake via the public
//! entry points.
//!
//! Each case exercises one of: client request build, server
//! response build, client request parse, server response
//! parse, key derivation/verification, or a reject path.
//! The RFC 6455 § 1.3 reference vector lock is the
//! single most-important assertion.

use grpcx_ws::{
    Handshake, WsError, build_server_handshake_response, derive_accept_key,
    parse_client_handshake_request, parse_server_handshake_response,
    perform_client_handshake_request, verify_accept_key,
};

// ============================================================
// RFC 6455 § 1.3 reference vector lock.
// ============================================================

#[test]
fn rfc6455_section_1_3_reference_vector() {
    // The canonical example: client sends
    // `Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==`, server
    // must respond with `Sec-WebSocket-Accept:
    // s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`.
    let req_key = "dGhlIHNhbXBsZSBub25jZQ==";
    let expected = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
    assert_eq!(derive_accept_key(req_key), expected);
    assert!(verify_accept_key(req_key, expected).is_ok());
}

// ============================================================
// Client request build.
// ============================================================

#[test]
fn client_request_simple() {
    let hs = Handshake {
        request_key: "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ..Handshake::default()
    };
    let req = perform_client_handshake_request("example.com", "/chat", &hs);
    assert!(req.starts_with("GET /chat HTTP/1.1\r\n"));
    assert!(req.contains("Host: example.com\r\n"));
    assert!(req.contains("Upgrade: websocket\r\n"));
    assert!(req.contains("Connection: Upgrade\r\n"));
    assert!(req.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
    assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
    assert!(req.ends_with("\r\n\r\n"));
}

#[test]
fn client_request_with_subprotocols_and_extensions() {
    let hs = Handshake {
        request_key: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
        subprotocols: vec!["chat".into(), "superchat".into()],
        extensions: vec!["permessage-deflate".into()],
        ..Handshake::default()
    };
    let req = perform_client_handshake_request("server.example.com", "/", &hs);
    assert!(req.contains("Sec-WebSocket-Protocol: chat, superchat\r\n"));
    assert!(req.contains("Sec-WebSocket-Extensions: permessage-deflate\r\n"));
}

// ============================================================
// Server response build.
// ============================================================

#[test]
fn server_response_simple() {
    let hs = Handshake {
        accept_key: "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".into(),
        ..Handshake::default()
    };
    let resp = build_server_handshake_response(&hs);
    assert!(resp.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
    assert!(resp.contains("Upgrade: websocket\r\n"));
    assert!(resp.contains("Connection: Upgrade\r\n"));
    assert!(resp.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
    assert!(resp.ends_with("\r\n\r\n"));
}

#[test]
fn server_response_with_selected_subprotocol() {
    let hs = Handshake {
        accept_key: "AAAA".into(),
        subprotocols: vec!["chat".into()],
        ..Handshake::default()
    };
    let resp = build_server_handshake_response(&hs);
    assert!(resp.contains("Sec-WebSocket-Protocol: chat\r\n"));
}

// ============================================================
// Round-trip via parse.
// ============================================================

#[test]
fn round_trip_client_request_build_then_parse() {
    let hs = Handshake {
        request_key: "dGhlIHNhbXBsZSBub25jZQ==".into(),
        subprotocols: vec!["chat".into()],
        extensions: vec!["permessage-deflate".into()],
        ..Handshake::default()
    };
    let req = perform_client_handshake_request("example.com", "/chat", &hs);
    let parsed = parse_client_handshake_request(req.as_bytes()).unwrap();
    assert_eq!(parsed.request_key, "dGhlIHNhbXBsZSBub25jZQ==");
    assert_eq!(parsed.subprotocols, vec!["chat".to_string()]);
    assert_eq!(parsed.extensions, vec!["permessage-deflate".to_string()]);
}

#[test]
fn round_trip_server_response_build_then_parse() {
    let hs = Handshake {
        accept_key: "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".into(),
        subprotocols: vec!["chat".into()],
        ..Handshake::default()
    };
    let resp = build_server_handshake_response(&hs);
    let parsed = parse_server_handshake_response(resp.as_bytes()).unwrap();
    assert_eq!(parsed.accept_key, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    assert_eq!(parsed.subprotocols, vec!["chat".to_string()]);
}

// ============================================================
// verify_accept_key.
// ============================================================

#[test]
fn verify_accept_key_mismatch_rejected() {
    let req_key = "dGhlIHNhbXBsZSBub25jZQ==";
    let wrong_accept = "AAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let err = verify_accept_key(req_key, wrong_accept).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

// ============================================================
// Reject paths — client request parser.
// ============================================================

#[test]
fn reject_client_missing_upgrade() {
    let req = "GET /chat HTTP/1.1\r\n\
               Host: example.com\r\n\
               Connection: Upgrade\r\n\
               Sec-WebSocket-Key: AAAA\r\n\
               Sec-WebSocket-Version: 13\r\n\
               \r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("Upgrade")));
}

#[test]
fn reject_client_upgrade_not_websocket() {
    let req = "GET /chat HTTP/1.1\r\n\
               Host: example.com\r\n\
               Upgrade: h2c\r\n\
               Connection: Upgrade\r\n\
               Sec-WebSocket-Key: AAAA\r\n\
               Sec-WebSocket-Version: 13\r\n\
               \r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn reject_client_missing_key() {
    let req = "GET /chat HTTP/1.1\r\n\
               Host: example.com\r\n\
               Upgrade: websocket\r\n\
               Connection: Upgrade\r\n\
               Sec-WebSocket-Version: 13\r\n\
               \r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("Sec-WebSocket-Key")));
}

#[test]
fn reject_client_wrong_version() {
    let req = "GET /chat HTTP/1.1\r\n\
               Host: example.com\r\n\
               Upgrade: websocket\r\n\
               Connection: Upgrade\r\n\
               Sec-WebSocket-Key: AAAA\r\n\
               Sec-WebSocket-Version: 12\r\n\
               \r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("Version")));
}

// ============================================================
// Reject paths — server response parser.
// ============================================================

#[test]
fn reject_server_wrong_status() {
    let resp = "HTTP/1.1 200 OK\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                Sec-WebSocket-Accept: xxx\r\n\
                \r\n";
    let err = parse_server_handshake_response(resp.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("101")));
}

#[test]
fn reject_server_missing_accept() {
    let resp = "HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                \r\n";
    let err = parse_server_handshake_response(resp.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("Sec-WebSocket-Accept")));
}

#[test]
fn reject_server_connection_missing_upgrade_token() {
    let resp = "HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: keep-alive\r\n\
                Sec-WebSocket-Accept: xxx\r\n\
                \r\n";
    let err = parse_server_handshake_response(resp.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(msg) if msg.contains("Upgrade")));
}

// ============================================================
// Structural rejects.
// ============================================================

#[test]
fn reject_no_separator() {
    let req = "GET /chat HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

#[test]
fn reject_malformed_start_line() {
    let req = "HELLO_WORLD\r\n\r\n";
    let err = parse_client_handshake_request(req.as_bytes()).unwrap_err();
    assert!(matches!(err, WsError::BadHandshake(_)));
}

// ============================================================
// Case-insensitive header acceptance.
// ============================================================

#[test]
fn accepts_case_insensitive_headers() {
    // `upgrade:` lowercase header name + `WebSocket`
    // mixed-case value. `Connection: Keep-Alive, upgrade`
    // checks token-list contains-check is also case-
    // insensitive.
    let req = "GET /chat HTTP/1.1\r\n\
               host: example.com\r\n\
               upgrade: WebSocket\r\n\
               Connection: Keep-Alive, upgrade\r\n\
               sec-websocket-key: AAAA\r\n\
               sec-websocket-version: 13\r\n\
               \r\n";
    let parsed = parse_client_handshake_request(req.as_bytes()).unwrap();
    assert_eq!(parsed.request_key, "AAAA");
}

// ============================================================
// Trailing body / pipelined bytes ignored (parser stops at
// the first \r\n\r\n).
// ============================================================

#[test]
fn trailing_bytes_after_headers_ignored() {
    let req = "GET /chat HTTP/1.1\r\n\
               Host: example.com\r\n\
               Upgrade: websocket\r\n\
               Connection: Upgrade\r\n\
               Sec-WebSocket-Key: AAAA\r\n\
               Sec-WebSocket-Version: 13\r\n\
               \r\nWHATEVER";
    let parsed = parse_client_handshake_request(req.as_bytes()).unwrap();
    assert_eq!(parsed.request_key, "AAAA");
}
