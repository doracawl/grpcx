//! ws-mux as a full transport (charter 2026-06-13): real
//! TCP sockets through `connect_ws_mux` ↔
//! `listen_ws_mux_until` (the threaded L2-Runtime serve
//! loop), all four RPC shapes over the shared mux
//! streaming core, plus the deadline plane riding along.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grpcx::{GrpcTimeout, Metadata, Method, RpcError, RpcKind, Server, Service, StatusCode};

struct Shapes;

impl Service for Shapes {
    fn name(&self) -> &str {
        "test.v1.Shapes"
    }
    fn dispatch(
        &self,
        method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        match method {
            "Echo" => {
                if request == b"slow" {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                Ok((request.to_vec(), Metadata::default()))
            }
            _ => Err(RpcError::Status(grpcx::Status {
                code: StatusCode::Unimplemented,
                message: format!("unknown method {method}"),
            })),
        }
    }
    fn dispatch_streaming(
        &self,
        method: &str,
        metadata: &Metadata,
        requests: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, Metadata), RpcError> {
        match method {
            "Echo" => {
                let first = requests.first().cloned().unwrap_or_default();
                let (resp, meta) = self.dispatch(method, metadata, &first)?;
                Ok((vec![resp], meta))
            }
            // Server-streaming: three copies of the request.
            "Fanout" => {
                let req = requests.first().cloned().unwrap_or_default();
                Ok((
                    (0..3)
                        .map(|i| {
                            let mut m = req.clone();
                            m.push(b'0' + i);
                            m
                        })
                        .collect(),
                    Metadata::default(),
                ))
            }
            // Client-streaming: concatenation.
            "Collect" => Ok((vec![requests.concat()], Metadata::default())),
            // Bidi: per-message echo with a prefix.
            "Chat" => Ok((
                requests
                    .iter()
                    .map(|r| [b"re:".as_slice(), r].concat())
                    .collect(),
                Metadata::default(),
            )),
            _ => Err(RpcError::Status(grpcx::Status {
                code: StatusCode::Unimplemented,
                message: format!("unknown method {method}"),
            })),
        }
    }
}

fn method(name: &str, kind: RpcKind) -> Method {
    Method {
        service: "test.v1.Shapes".to_string(),
        method: name.to_string(),
        kind,
    }
}

fn start_server() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let handle = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(Shapes));
        server
            .listen_ws_mux_until(&server_addr, &server_stop)
            .unwrap();
    });
    (addr, stop, handle)
}

fn connect_with_retry(addr: &str) -> grpcx::ConnectedChannel {
    for _ in 0..200 {
        if let Ok(c) = grpcx::ConnectedChannel::connect_ws_mux(addr, addr) {
            return c;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("ws-mux server at {addr} never came up");
}

#[test]
fn all_four_rpc_shapes_over_real_sockets() {
    let (addr, stop, handle) = start_server();
    let mut conn = connect_with_retry(&addr);

    // Unary.
    let (resp, _) = conn
        .call_unary(
            &method("Echo", RpcKind::Unary),
            &Metadata::default(),
            b"ping",
        )
        .expect("unary");
    assert_eq!(resp, b"ping");

    // Server-streaming.
    let (msgs, _) = conn
        .call_server_streaming(
            &method("Fanout", RpcKind::ServerStreaming),
            &Metadata::default(),
            b"x",
        )
        .expect("server streaming");
    assert_eq!(msgs, vec![b"x0".to_vec(), b"x1".to_vec(), b"x2".to_vec()]);

    // Client-streaming.
    let reqs: [&[u8]; 3] = [b"a", b"b", b"c"];
    let (resp, _) = conn
        .call_client_streaming(
            &method("Collect", RpcKind::ClientStreaming),
            &Metadata::default(),
            &reqs,
        )
        .expect("client streaming");
    assert_eq!(resp, b"abc");

    // Bidi.
    let reqs: [&[u8]; 2] = [b"hi", b"yo"];
    let (msgs, _) = conn
        .call_bidi(
            &method("Chat", RpcKind::BidirectionalStreaming),
            &Metadata::default(),
            &reqs,
        )
        .expect("bidi");
    assert_eq!(msgs, vec![b"re:hi".to_vec(), b"re:yo".to_vec()]);

    // Error status propagates.
    match conn.call_unary(&method("Nope", RpcKind::Unary), &Metadata::default(), b"") {
        Err(RpcError::Status(s)) => assert_eq!(s.code, StatusCode::Unimplemented),
        other => panic!("expected Unimplemented, got {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn deadline_is_a_call_verdict_over_ws_mux() {
    let (addr, stop, handle) = start_server();
    let mut conn = connect_with_retry(&addr);

    let meta = Metadata {
        headers: vec![GrpcTimeout::from_duration(std::time::Duration::from_millis(50)).to_header()],
    };
    let t0 = std::time::Instant::now();
    match conn.call_unary(&method("Echo", RpcKind::Unary), &meta, b"slow") {
        Err(RpcError::Status(s)) => assert_eq!(s.code, StatusCode::DeadlineExceeded),
        other => panic!("expected DEADLINE_EXCEEDED, got {other:?}"),
    }
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(200),
        "deadline must fire near 50 ms"
    );

    // Same connection, next call succeeds.
    let (resp, _) = conn
        .call_unary(
            &method("Echo", RpcKind::Unary),
            &Metadata::default(),
            b"after",
        )
        .expect("connection must survive a deadline-exceeded call");
    assert_eq!(resp, b"after");

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}
