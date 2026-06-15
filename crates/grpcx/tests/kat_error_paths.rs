//! v1.x-0 — error-arm KATs closing the coverage gap left
//! at v10.0 (transport error mapping, panic guards,
//! injected protocol violations).

use grpcx::{
    Channel, DatagramTransport, GrpcTimeout, Metadata, Method, RpcError, RpcKind, Server, Service,
    TimeoutUnit, encode_header_list, encode_mux_frame,
};

fn method(name: &str) -> Method {
    Method {
        service: "test.v1.Echo".to_string(),
        method: name.to_string(),
        kind: RpcKind::Unary,
    }
}

// ============================================================
// Panic guards (documented contract violations).
// ============================================================

#[test]
#[should_panic(expected = "8 decimal digits")]
fn grpc_timeout_over_8_digits_panics() {
    let _ = GrpcTimeout {
        value: 100_000_000,
        unit: TimeoutUnit::Nanos,
    }
    .to_header();
}

#[test]
#[should_panic(expected = "exceeds u16")]
fn header_list_oversized_value_panics() {
    let meta = Metadata {
        headers: vec![("k".to_string(), vec![0u8; 70_000])],
    };
    let _ = encode_header_list(&meta);
}

// ============================================================
// Transport error mapping — garbage into the pumps.
// ============================================================

#[test]
fn h3_pump_garbage_datagram_is_absorbed() {
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);
    let mut ch = Channel::connect_h3(&client_cfg, "x.example", "x.example:443").unwrap();
    // Feed a non-QUIC datagram. RFC 9000 lets endpoints
    // drop unparseable packets: the pump must neither
    // panic nor poison the session — the handshake keeps
    // emitting from a healthy state machine.
    let r = ch.pump(Some(&[0xFF; 32]));
    assert!(r.is_ok(), "garbage must be absorbed, got {r:?}");
    assert!(ch.pump(None).is_ok());
    assert!(!ch.is_ready()); // still pre-handshake, not wedged
}

#[test]
fn ws_server_garbage_after_upgrade_is_transport_error() {
    // Drive a real upgrade first, then inject a malformed
    // WS frame.
    let mut ch = Channel::connect_ws_mux("h", "h:443", "dGhlIHNhbXBsZSBub25jZQ==", 3);
    let server = Server::new();
    let mut conn = server.accept_ws_mux();

    let out = ch.pump_ws(None).unwrap(); // upgrade request
    let mut resp = Vec::new();
    for c in &out {
        resp.extend(conn.pump(Some(c)).unwrap());
    }
    for c in &resp {
        ch.pump_ws(Some(c)).unwrap();
    }
    assert!(ch.is_ready_ws() && conn.is_ready());

    // RSV bits set → L9 rejects → transport error.
    let r = conn.pump(Some(&[0xF2, 0x00]));
    assert!(matches!(r, Err(RpcError::Transport(_))), "got {r:?}");
}

// ============================================================
// Injected mux protocol violations (server side).
// ============================================================

struct Echo;
impl Service for Echo {
    fn name(&self) -> &str {
        "test.v1.Echo"
    }
    fn dispatch(
        &self,
        _m: &str,
        _md: &Metadata,
        r: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Ok((r.to_vec(), Metadata::default()))
    }
}

/// Established ws pair + helper to inject raw mux frames
/// as masked client binary messages.
fn ws_pair() -> (Channel, grpcx::WsMuxServerConn, Server) {
    let mut ch = Channel::connect_ws_mux("h", "h:443", "dGhlIHNhbXBsZSBub25jZQ==", 9);
    let mut server = Server::new();
    server.register(Box::new(Echo));
    let mut conn = server.accept_ws_mux();
    let out = ch.pump_ws(None).unwrap();
    let mut resp = Vec::new();
    for c in &out {
        resp.extend(conn.pump(Some(c)).unwrap());
    }
    for c in &resp {
        ch.pump_ws(Some(c)).unwrap();
    }
    assert!(ch.is_ready_ws() && conn.is_ready());
    (ch, conn, server)
}

fn inject(conn: &mut grpcx::WsMuxServerConn, mux: Vec<u8>) -> Result<Vec<Vec<u8>>, RpcError> {
    let mut ws = Vec::new();
    grpcx_ws::encode_frame(
        &grpcx_ws::Frame {
            fin: true,
            rsv1: false,
            rsv2: false,
            rsv3: false,
            opcode: grpcx_ws::Opcode::Binary,
            mask: Some([1, 2, 3, 4]),
            payload: mux,
        },
        &mut ws,
    );
    conn.pump(Some(&ws))
}

#[test]
fn ws_third_headers_section_is_dropped_and_extra_trailers_ignored() {
    let (_ch, mut conn, server) = ws_pair();
    // Stream 1: three HEADERS sections + an extra TRAILERS.
    let hdrs = encode_header_list(&Metadata {
        headers: vec![(":method".to_string(), b"POST".to_vec())],
    });
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, false, &hdrs),
    )
    .unwrap();
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, false, &hdrs),
    )
    .unwrap();
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, false, &hdrs),
    )
    .unwrap();
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_TRAILERS, false, &hdrs),
    )
    .unwrap();
    // No END_STREAM yet → nothing dispatches; no panic, no
    // error: the violations are absorbed.
    assert_eq!(conn.dispatch(&server).unwrap(), 0);
}

#[test]
fn ws_malformed_request_headers_get_error_response() {
    let (_ch, mut conn, server) = ws_pair();
    // HEADERS missing :method/:path + END_STREAM → server
    // must answer trailers-only InvalidArgument rather than
    // fail the connection.
    let bad = encode_header_list(&Metadata {
        headers: vec![("x".to_string(), b"y".to_vec())],
    });
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, true, &bad),
    )
    .unwrap();
    assert_eq!(conn.dispatch(&server).unwrap(), 1);
    // The error response is queued for the client.
    let out = conn.pump(None).unwrap();
    assert!(!out.is_empty());
}

#[test]
fn ws_bad_grpc_frame_in_body_gets_error_response() {
    let (_ch, mut conn, server) = ws_pair();
    let hdrs = encode_header_list(&grpcx::encode_request_headers(
        "test.v1.Echo",
        "Echo",
        "h:443",
        &Metadata::default(),
    ));
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, false, &hdrs),
    )
    .unwrap();
    // DATA carrying an invalid gRPC frame (bad flag byte).
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_DATA, true, &[7, 0, 0, 0, 0]),
    )
    .unwrap();
    assert_eq!(conn.dispatch(&server).unwrap(), 1);
}

#[test]
fn ws_compressed_grpc_frame_gets_unimplemented() {
    let (_ch, mut conn, server) = ws_pair();
    let hdrs = encode_header_list(&grpcx::encode_request_headers(
        "test.v1.Echo",
        "Echo",
        "h:443",
        &Metadata::default(),
    ));
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_HEADERS, false, &hdrs),
    )
    .unwrap();
    // Valid frame with compressed-flag = 1.
    inject(
        &mut conn,
        encode_mux_frame(1, grpcx::MUX_DATA, true, &[1, 0, 0, 0, 0]),
    )
    .unwrap();
    assert_eq!(conn.dispatch(&server).unwrap(), 1);
}

// ============================================================
// Empty-response unary (the None → Vec::new() arm).
// ============================================================

struct Empty;
impl Service for Empty {
    fn name(&self) -> &str {
        "test.v1.Empty"
    }
    fn dispatch(
        &self,
        _m: &str,
        _md: &Metadata,
        _r: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Ok((Vec::new(), Metadata::default()))
    }
    fn dispatch_streaming(
        &self,
        _m: &str,
        _md: &Metadata,
        _reqs: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        // ZERO response messages — exercises the
        // empty-frames decode arm on the client.
        Ok((Vec::new(), Metadata::default()))
    }
}

#[test]
fn ws_zero_message_response_yields_empty_payload() {
    let mut ch = Channel::connect_ws_mux("h", "h:443", "dGhlIHNhbXBsZSBub25jZQ==", 11);
    let mut server = Server::new();
    server.register(Box::new(Empty));
    let mut conn = server.accept_ws_mux();
    // establish
    let out = ch.pump_ws(None).unwrap();
    let mut resp = Vec::new();
    for c in &out {
        resp.extend(conn.pump(Some(c)).unwrap());
    }
    for c in &resp {
        ch.pump_ws(Some(c)).unwrap();
    }

    let call = ch
        .begin_unary_ws(
            &Method {
                service: "test.v1.Empty".to_string(),
                method: "Nop".to_string(),
                kind: RpcKind::Unary,
            },
            &Metadata::default(),
            b"x",
        )
        .unwrap();
    for _ in 0..32 {
        // shuttle
        let outc = ch.pump_ws(None).unwrap();
        let mut back = Vec::new();
        for c in &outc {
            back.extend(conn.pump(Some(c)).unwrap());
        }
        conn.dispatch(&server).unwrap();
        back.extend(conn.pump(None).unwrap());
        for c in &back {
            ch.pump_ws(Some(c)).unwrap();
        }
        if let Some((payload, _)) = ch.poll_unary_ws(&call).unwrap() {
            assert!(payload.is_empty());
            return;
        }
    }
    panic!("empty-response call did not complete");
}

// ============================================================
// Fuzz-driver smoke: the no-panic invariant holds on a
// seed corpus (the cargo-fuzz harness runs the same
// drivers on mutated inputs).
// ============================================================

#[test]
fn fuzz_drivers_no_panic_on_seed_corpus() {
    use grpcx::__fuzz_helpers as fz;
    let seeds: &[&[u8]] = &[
        b"",
        &[0x00],
        &[0x01, 0x00, 0x00, 0x00, 0x00],
        &[0xFF; 64],
        b"\x00\x00\x00\x07\x0a\x05world",
        &[0x00, 0x00, 0x00, 0x01, 0x82, b'%', b'4', b'1'],
    ];
    for s in seeds {
        fz::drive_frame(s);
        fz::drive_percent(s);
        fz::drive_mux(s);
    }
    fz::drive_request_headers(&[
        (":method".to_string(), b"POST".to_vec()),
        ("grpc-status".to_string(), b"5".to_vec()),
        ("grpc-message".to_string(), vec![0xFF, b'%', b'g']),
    ]);
    fz::drive_request_headers(&[]);
}

// ============================================================
// Blocking facade stall guard.
// ============================================================

struct DeadTransport;
impl DatagramTransport for DeadTransport {
    fn send(&mut self, _d: &[u8]) -> Result<(), RpcError> {
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(None)
    }
}

#[test]
fn call_unary_reports_link_dead_on_dead_transport() {
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);
    let mut ch = Channel::connect_h3(&client_cfg, "x.example", "x.example:443").unwrap();
    // Liveness is the transport's verdict (0.17): shrink
    // the window so the dead link is declared fast.
    ch.set_link_liveness(
        core::time::Duration::from_millis(10),
        core::time::Duration::from_millis(50),
    )
    .unwrap();
    let r = grpcx::call_unary(
        &mut ch,
        &mut DeadTransport,
        &method("Echo"),
        &Metadata::default(),
        b"",
    );
    assert!(
        matches!(r, Err(RpcError::Transport("h3: link dead"))),
        "got {r:?}"
    );
}

#[test]
fn call_unary_reports_link_dead_on_dead_transport_h2c() {
    let mut ch = Channel::connect_h2c_session("x.example:443");
    ch.set_link_liveness(
        core::time::Duration::from_millis(10),
        core::time::Duration::from_millis(50),
    )
    .unwrap();
    let r = grpcx::call_unary(
        &mut ch,
        &mut DeadTransport,
        &method("Echo"),
        &Metadata::default(),
        b"",
    );
    assert!(
        matches!(r, Err(RpcError::Transport("h2: link dead"))),
        "got {r:?}"
    );
}

#[test]
fn call_unary_reports_link_dead_on_dead_transport_tcp_mux() {
    let mut ch = Channel::connect_tcp_mux_session("x.example:443");
    ch.set_link_liveness(
        core::time::Duration::from_millis(10),
        core::time::Duration::from_millis(50),
    )
    .unwrap();
    let r = grpcx::call_unary(
        &mut ch,
        &mut DeadTransport,
        &method("Echo"),
        &Metadata::default(),
        b"",
    );
    assert!(
        matches!(r, Err(RpcError::Transport("tcp-mux: link dead"))),
        "got {r:?}"
    );
}
