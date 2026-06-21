//! End-to-end tests for `transport-tcp-mux v2` — wire
//! format v2 over raw TCP. Covers the four RPC kinds,
//! application Status errors, and request-header
//! round-trip. No peer-credential test — TCP carries no
//! kernel-attested identity.

#![cfg(feature = "transport-tcp-mux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{
    CallContext, Metadata, RpcError, Server, Service, Status, StatusCode, TcpMuxV2Connection,
};

// ============================================================
// Test service — exposes one method per RPC kind plus a
// few cross-cutting behaviours (Status, metadata round-
// trip).
// ============================================================

struct TestService;

impl Service for TestService {
    fn name(&self) -> &str {
        "tcp_mux_v2_test.v1.Echo"
    }

    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        _request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        // Unused — all calls go through dispatch_with_ctx.
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "dispatch should not be called on TestService".to_string(),
        }))
    }

    fn dispatch_with_ctx(
        &self,
        method: &str,
        ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        match method {
            "unary_echo" => {
                ctx.add_trailing("x-handler", b"echo");
                Ok(vec![requests.first().cloned().unwrap_or_default()])
            }
            "server_stream_fanout" => {
                let base = requests.first().cloned().unwrap_or_default();
                Ok((0..3)
                    .map(|i| {
                        let mut out = base.clone();
                        out.push(b'#');
                        out.push(b'0' + i);
                        out
                    })
                    .collect())
            }
            "client_stream_concat" => {
                let mut out = Vec::new();
                for r in requests {
                    if !out.is_empty() {
                        out.push(b'+');
                    }
                    out.extend_from_slice(r);
                }
                Ok(vec![out])
            }
            "bidi_reply" => Ok(requests
                .iter()
                .map(|r| {
                    let mut out = b"re:".to_vec();
                    out.extend_from_slice(r);
                    out
                })
                .collect()),
            "denied" => Err(RpcError::Status(Status {
                code: StatusCode::PermissionDenied,
                message: "blocked by test".to_string(),
            })),
            "echo_request_header" => {
                let v = ctx
                    .request_metadata()
                    .headers
                    .iter()
                    .find(|(n, _)| n == "x-request-id")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                Ok(vec![v])
            }
            "peer_cred_is_none" => {
                // TCP carries no kernel-attested identity.
                // Confirm the server-side handler sees
                // ctx.peer_cred() == None on this
                // transport.
                if ctx.peer_cred().is_some() {
                    return Err(RpcError::Status(Status {
                        code: StatusCode::FailedPrecondition,
                        message: "peer_cred should be None on tcp-mux-v2".to_string(),
                    }));
                }
                Ok(vec![b"ok".to_vec()])
            }
            other => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("method {other} not in TestService"),
            })),
        }
    }
}

// ============================================================
// Test harness — ephemeral-port loopback.
// ============================================================

/// Bind a fresh ephemeral loopback port, return the
/// resolved `127.0.0.1:port` string. We bind here so the
/// kernel hands back a free port; the server thread then
/// rebinds the same address (no race because `SO_REUSEADDR`
/// is the kernel default after a clean close, and we drop
/// our listener immediately).
fn ephemeral_loopback_addr() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().expect("local_addr").port();
    drop(l);
    format!("127.0.0.1:{port}")
}

/// Spawn a server bound at `addr` and return a stop
/// handle.
fn spawn_server(addr: String) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let h = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(TestService));
        let _ = server.listen_tcp_mux_v2_until(&addr, &stop_clone);
    });
    (stop, h)
}

/// Connect with retry — the server thread binds after
/// spawn, so the first attempts can race it.
fn connect_with_retry(addr: &str) -> TcpMuxV2Connection {
    for _ in 0..200 {
        if let Ok(c) = TcpMuxV2Connection::connect(addr, "localhost") {
            return c;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("tcp-mux-v2 server at {addr} never came up")
}

/// Clean shutdown — flip stop + join the server thread.
fn shutdown(stop: Arc<AtomicBool>, h: std::thread::JoinHandle<()>) {
    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
}

// ============================================================
// 1) Unary round-trip + trailing metadata
// ============================================================

#[test]
fn unary_round_trip_with_trailing_metadata() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let (body, trailing) = conn
        .call_unary(
            "tcp_mux_v2_test.v1.Echo",
            "unary_echo",
            &Metadata::default(),
            b"hello",
        )
        .expect("unary call");
    assert_eq!(body, b"hello");
    assert!(
        trailing
            .headers
            .iter()
            .any(|(n, v)| n == "x-handler" && v == b"echo"),
        "trailing meta should include x-handler=echo, got {:?}",
        trailing
    );

    shutdown(stop, h);
}

// ============================================================
// 2) Server-streaming — one request, three responses
// ============================================================

#[test]
fn server_streaming_fan_out_3() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let (bodies, _trailing) = conn
        .call_server_streaming(
            "tcp_mux_v2_test.v1.Echo",
            "server_stream_fanout",
            &Metadata::default(),
            b"fan",
        )
        .expect("server_streaming call");
    let strs: Vec<_> = bodies
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(strs, ["fan#0", "fan#1", "fan#2"]);

    shutdown(stop, h);
}

// ============================================================
// 3) Client-streaming — three requests, one response
// ============================================================

#[test]
fn client_streaming_concat() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (body, _trailing) = conn
        .call_client_streaming(
            "tcp_mux_v2_test.v1.Echo",
            "client_stream_concat",
            &Metadata::default(),
            &reqs,
        )
        .expect("client_streaming call");
    assert_eq!(body, b"a+b+c");

    shutdown(stop, h);
}

// ============================================================
// 4) Bidi-streaming — three in, three out
// ============================================================

#[test]
fn bidi_three_each_way() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (bodies, _trailing) = conn
        .call_bidi(
            "tcp_mux_v2_test.v1.Echo",
            "bidi_reply",
            &Metadata::default(),
            &reqs,
        )
        .expect("bidi call");
    let strs: Vec<_> = bodies
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(strs, ["re:a", "re:b", "re:c"]);

    shutdown(stop, h);
}

// ============================================================
// 5) Application Status — PermissionDenied surfaces as
//    RpcError::Status to the client
// ============================================================

#[test]
fn status_error_round_trip() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let err = conn
        .call_unary(
            "tcp_mux_v2_test.v1.Echo",
            "denied",
            &Metadata::default(),
            b"x",
        )
        .expect_err("denied should fail");
    match err {
        RpcError::Status(s) => {
            assert_eq!(s.code, StatusCode::PermissionDenied);
            assert_eq!(s.message, "blocked by test");
        }
        other => panic!("expected RpcError::Status, got {other:?}"),
    }

    shutdown(stop, h);
}

// ============================================================
// 6) Request header round-trip
// ============================================================

#[test]
fn request_header_visible_to_handler() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let mut meta = Metadata::default();
    meta.headers
        .push(("x-request-id".to_string(), b"abc-123".to_vec()));
    let (body, _trailing) = conn
        .call_unary("tcp_mux_v2_test.v1.Echo", "echo_request_header", &meta, &[])
        .expect("call");
    assert_eq!(body, b"abc-123");

    shutdown(stop, h);
}

// ============================================================
// 7) peer_cred is None on TCP — the parallel-pipeline
//    delta from UDS / shmem.
// ============================================================

#[test]
fn peer_cred_is_none_on_tcp() {
    let addr = ephemeral_loopback_addr();
    let (stop, h) = spawn_server(addr.clone());
    let mut conn = connect_with_retry(&addr);

    let (body, _trailing) = conn
        .call_unary(
            "tcp_mux_v2_test.v1.Echo",
            "peer_cred_is_none",
            &Metadata::default(),
            &[],
        )
        .expect("call");
    assert_eq!(body, b"ok");

    shutdown(stop, h);
}
