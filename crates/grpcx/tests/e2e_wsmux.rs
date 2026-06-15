//! L10 v0.6 acceptance — round-trip unary over
//! `grpcx/ws-mux` (work unit 6), plus the ALPN selection
//! + Alt-Svc pure functions and the mux frame codec.

use std::collections::VecDeque;

use grpcx::{
    ALPN_H3, ALPN_WS_MUX, Channel, Metadata, Method, RpcError, RpcKind, Server, Service, Status,
    StatusCode, TransportKind, build_alt_svc_header, decode_header_list, decode_mux_frame,
    encode_header_list, encode_mux_frame, parse_alt_svc_header, select_alpn_transport,
};

// ============================================================
// Echo service (same shape as the h3 e2e).
// ============================================================

struct Echo;

impl Service for Echo {
    fn name(&self) -> &str {
        "test.v1.Echo"
    }

    fn dispatch(
        &self,
        method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        match method {
            "Echo" => Ok((
                request.to_vec(),
                Metadata {
                    headers: vec![("x-echoed".to_string(), b"ws".to_vec())],
                },
            )),
            "Fail" => Err(RpcError::Status(Status {
                code: StatusCode::PermissionDenied,
                message: "denied: 王".to_string(),
            })),
            _ => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: String::new(),
            })),
        }
    }
}

fn method(name: &str) -> Method {
    Method {
        service: "test.v1.Echo".to_string(),
        method: name.to_string(),
        kind: RpcKind::Unary,
    }
}

// ============================================================
// Loopback harness — byte-stream flavoured (TCP-ish):
// chunks are concatenated arbitrarily to exercise the
// incremental WS frame parser.
// ============================================================

struct Loopback {
    channel: Channel,
    conn: grpcx::WsMuxServerConn,
    server: Server,
    to_server: VecDeque<Vec<u8>>,
    to_client: VecDeque<Vec<u8>>,
}

impl Loopback {
    fn new() -> Self {
        let channel = Channel::connect_ws_mux(
            "loopback.example",
            "loopback.example:443",
            "dGhlIHNhbXBsZSBub25jZQ==",
            0x5EED,
        );
        let mut server = Server::new();
        server.register(Box::new(Echo));
        let conn = server.accept_ws_mux();
        Loopback {
            channel,
            conn,
            server,
            to_server: VecDeque::new(),
            to_client: VecDeque::new(),
        }
    }

    fn cycle(&mut self) {
        let incoming = self.to_client.pop_front();
        for c in self.channel.pump_ws(incoming.as_deref()).unwrap() {
            self.to_server.push_back(c);
        }
        let incoming = self.to_server.pop_front();
        for c in self.conn.pump(incoming.as_deref()).unwrap() {
            self.to_client.push_back(c);
        }
        self.conn.dispatch(&self.server).unwrap();
    }

    fn establish(&mut self) {
        for _ in 0..16 {
            self.cycle();
            if self.channel.is_ready_ws() && self.conn.is_ready() {
                return;
            }
        }
        panic!("ws upgrade did not complete");
    }
}

#[test]
fn ws_mux_unary_echo_round_trip() {
    let mut lb = Loopback::new();
    lb.establish();
    assert_eq!(lb.channel.transport(), TransportKind::WsMux);

    let call = lb
        .channel
        .begin_unary_ws(
            &method("Echo"),
            &Metadata {
                headers: vec![("x-req".to_string(), b"1".to_vec())],
            },
            b"ws-ping",
        )
        .unwrap();

    for _ in 0..64 {
        lb.cycle();
        if let Some((payload, trailing)) = lb.channel.poll_unary_ws(&call).unwrap() {
            assert_eq!(payload, b"ws-ping");
            assert!(
                trailing
                    .headers
                    .iter()
                    .any(|(n, v)| n == "x-echoed" && v == b"ws")
            );
            return;
        }
    }
    panic!("ws-mux unary did not complete");
}

#[test]
fn ws_mux_error_status_propagates() {
    let mut lb = Loopback::new();
    lb.establish();

    let call = lb
        .channel
        .begin_unary_ws(&method("Fail"), &Metadata::default(), b"")
        .unwrap();

    for _ in 0..64 {
        lb.cycle();
        match lb.channel.poll_unary_ws(&call) {
            Ok(None) => continue,
            Ok(Some(_)) => panic!("Fail must not succeed"),
            Err(RpcError::Status(status)) => {
                assert_eq!(status.code, StatusCode::PermissionDenied);
                // UTF-8 message survives the percent
                // round-trip over ws-mux too.
                assert_eq!(status.message, "denied: 王");
                return;
            }
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }
    panic!("error did not propagate");
}

#[test]
fn ws_mux_concurrent_unary_calls_multiplex() {
    let mut lb = Loopback::new();
    lb.establish();

    // Two in-flight calls on one WS connection — the mux
    // layer keeps them apart by stream id.
    let a = lb
        .channel
        .begin_unary_ws(&method("Echo"), &Metadata::default(), b"call-a")
        .unwrap();
    let b = lb
        .channel
        .begin_unary_ws(&method("Echo"), &Metadata::default(), b"call-b")
        .unwrap();

    let (mut got_a, mut got_b) = (false, false);
    for _ in 0..128 {
        lb.cycle();
        if !got_a && let Some((p, _)) = lb.channel.poll_unary_ws(&a).unwrap() {
            assert_eq!(p, b"call-a");
            got_a = true;
        }
        if !got_b && let Some((p, _)) = lb.channel.poll_unary_ws(&b).unwrap() {
            assert_eq!(p, b"call-b");
            got_b = true;
        }
        if got_a && got_b {
            return;
        }
    }
    panic!("concurrent calls did not both complete (a={got_a}, b={got_b})");
}

// ============================================================
// ALPN selection + Alt-Svc.
// ============================================================

#[test]
fn alpn_selection_prefers_h3() {
    assert_eq!(
        select_alpn_transport(&[ALPN_H3, ALPN_WS_MUX], &[ALPN_WS_MUX, ALPN_H3]),
        Some(TransportKind::H3)
    );
    assert_eq!(
        select_alpn_transport(&[ALPN_WS_MUX], &[ALPN_H3, ALPN_WS_MUX]),
        Some(TransportKind::WsMux)
    );
    assert_eq!(
        select_alpn_transport(&[ALPN_H3], &[ALPN_WS_MUX]),
        None,
        "HTTP/2 is not a fallback; mismatch = no transport"
    );
    assert_eq!(select_alpn_transport(&[], &[ALPN_H3]), None);
}

#[test]
fn alt_svc_round_trip() {
    let h = build_alt_svc_header(4433, 86400);
    assert_eq!(h, "h3-grpcx=\":4433\"; ma=86400");
    assert_eq!(parse_alt_svc_header(&h), Some(4433));
    assert_eq!(parse_alt_svc_header("other=\":1\""), None);
    assert_eq!(parse_alt_svc_header("h3-grpcx=\":notaport\""), None);
}

// ============================================================
// Mux frame codec KAT.
// ============================================================

#[test]
fn mux_frame_round_trip_and_rejects() {
    let f = encode_mux_frame(7, grpcx::MUX_DATA, true, b"payload");
    let (sid, ty, end, payload) = decode_mux_frame(&f).unwrap();
    assert_eq!(
        (sid, ty, end, payload),
        (7, grpcx::MUX_DATA, true, &b"payload"[..])
    );

    // Wire layout is locked: BE stream id, then type|flags.
    assert_eq!(
        &f[..5],
        &[0, 0, 0, 7, grpcx::MUX_DATA | grpcx::MUX_END_STREAM]
    );

    assert!(decode_mux_frame(&[0, 0, 0]).is_err()); // short
    assert!(decode_mux_frame(&[0, 0, 0, 0, 0x02]).is_err()); // sid 0
    assert!(decode_mux_frame(&[0, 0, 0, 1, 0x7F]).is_err()); // unknown type
}

#[test]
fn header_list_round_trip() {
    let meta = Metadata {
        headers: vec![
            (":path".to_string(), b"/s/M".to_vec()),
            ("x-bin".to_string(), vec![0, 159, 146, 150]), // arbitrary bytes value
            ("empty".to_string(), Vec::new()),
        ],
    };
    let wire = encode_header_list(&meta);
    assert_eq!(decode_header_list(&wire).unwrap(), meta);

    // Truncation rejects.
    assert!(decode_header_list(&wire[..wire.len() - 1]).is_err());
    assert!(decode_header_list(&[0x00]).is_err());
}

// ============================================================
// Link liveness (0.17) at the ws-mux pump level: a quiet
// delivering link exchanges mux PING/PONG and stays up; a
// black-holed one dies within the configured idle timeout.
// ============================================================

#[test]
fn ws_mux_quiet_link_stays_up_via_keepalive() {
    let mut lb = Loopback::new();
    lb.establish();
    lb.channel
        .set_link_liveness(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(60),
        )
        .unwrap();
    lb.conn.set_link_liveness(
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(60),
    );

    // Idle (no RPCs) for 4 × the idle timeout, pumping on a
    // real-time cadence — pings/pongs must keep both ends
    // alive.
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(240) {
        lb.cycle();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // Still usable.
    let call = lb
        .channel
        .begin_unary_ws(&method("Echo"), &Metadata::default(), b"after-idle")
        .unwrap();
    for _ in 0..256 {
        lb.cycle();
        if let Some((payload, _)) = lb.channel.poll_unary_ws(&call).unwrap() {
            assert_eq!(payload, b"after-idle");
            return;
        }
    }
    panic!("post-idle ws-mux call did not complete");
}

#[test]
fn ws_mux_blackholed_link_dies_within_idle_timeout() {
    let mut lb = Loopback::new();
    lb.establish();
    lb.channel
        .set_link_liveness(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(50),
        )
        .unwrap();

    // Black hole: the client keeps pumping but nothing is
    // ever delivered again.
    let t0 = std::time::Instant::now();
    loop {
        match lb.channel.pump_ws(None) {
            Ok(_) => {
                assert!(
                    t0.elapsed() < std::time::Duration::from_millis(500),
                    "black-holed ws-mux link must die within the idle timeout"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(e) => {
                assert!(
                    matches!(e, RpcError::Transport("ws-mux: link dead")),
                    "got {e:?}"
                );
                return;
            }
        }
    }
}
