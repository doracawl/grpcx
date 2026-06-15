//! Per-call deadline acceptance (charter 2026-06-12,
//! plane 2 of the stall/deadline split): `grpc-timeout`
//! rides the request metadata; the blocking facades
//! enforce it locally; expiry is a CALL verdict
//! (`Status(DEADLINE_EXCEEDED)`), never a link verdict —
//! the connection stays usable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grpcx::{GrpcTimeout, Metadata, Method, RpcError, RpcKind, Server, Service, StatusCode};

/// Sleeps long when asked to (`payload == b"slow"`),
/// echoes fast otherwise.
struct SometimesSlow;

impl Service for SometimesSlow {
    fn name(&self) -> &str {
        "test.v1.SometimesSlow"
    }
    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        if request == b"slow" {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Ok((request.to_vec(), Metadata::default()))
    }
}

fn method() -> Method {
    Method {
        service: "test.v1.SometimesSlow".to_string(),
        method: "Echo".to_string(),
        kind: RpcKind::Unary,
    }
}

fn timeout_meta(timeout: std::time::Duration) -> Metadata {
    Metadata {
        headers: vec![GrpcTimeout::from_duration(timeout).to_header()],
    }
}

fn connect_with_retry(
    addr: &str,
    connect: fn(&str) -> Result<grpcx::ConnectedChannel, RpcError>,
) -> grpcx::ConnectedChannel {
    for _ in 0..200 {
        if let Ok(c) = connect(addr) {
            return c;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("server at {addr} never came up");
}

fn run_deadline_case(
    listen: impl FnOnce(Server, String, Arc<AtomicBool>) + Send + 'static,
    connect: fn(&str) -> Result<grpcx::ConnectedChannel, RpcError>,
) {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SometimesSlow));
        listen(server, server_addr, server_stop);
    });

    let mut conn = connect_with_retry(&addr, connect);

    // A 300 ms handler under a 50 ms deadline: the call
    // must come back DEADLINE_EXCEEDED near the deadline,
    // not after the handler.
    let t0 = std::time::Instant::now();
    let r = conn.call_unary(
        &method(),
        &timeout_meta(std::time::Duration::from_millis(50)),
        b"slow",
    );
    let elapsed = t0.elapsed();
    match r {
        Err(RpcError::Status(s)) => assert_eq!(s.code, StatusCode::DeadlineExceeded),
        other => panic!("expected DEADLINE_EXCEEDED, got {other:?}"),
    }
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "deadline must fire near 50 ms, not after the 300 ms handler (took {elapsed:?})"
    );

    // The deadline was a CALL verdict — the same
    // connection serves the next (fast, unbounded) call.
    let (resp, _) = conn
        .call_unary(&method(), &Metadata::default(), b"quick")
        .expect("connection must survive a deadline-exceeded call");
    assert_eq!(resp, b"quick");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn deadline_exceeded_is_a_call_verdict_h2c() {
    run_deadline_case(
        |server, addr, stop| server.listen_h2c_until(&addr, &stop).unwrap(),
        |addr| grpcx::ConnectedChannel::connect_h2c(addr, addr),
    );
}

#[test]
fn deadline_exceeded_is_a_call_verdict_tcp_mux() {
    run_deadline_case(
        |server, addr, stop| server.listen_tcp_mux_until(&addr, &stop).unwrap(),
        |addr| grpcx::ConnectedChannel::connect_tcp_mux(addr, addr),
    );
}

/// No deadline = gRPC semantics: the call waits the
/// handler out (the old 32 ms stall is gone; liveness
/// keeps the link proven alive meanwhile).
#[test]
fn no_deadline_waits_out_a_slow_handler() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SometimesSlow));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    let mut conn = connect_with_retry(&addr, |a| grpcx::ConnectedChannel::connect_h2c(a, a));
    let (resp, _) = conn
        .call_unary(&method(), &Metadata::default(), b"slow")
        .expect("unbounded call must wait the handler out");
    assert_eq!(resp, b"slow");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
