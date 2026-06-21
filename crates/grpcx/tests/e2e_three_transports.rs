//! T5.2 — Cross-transport e2e (Rust).
//!
//! A single `Server` registers one `Greeter` impl and
//! serves it simultaneously on all three internal-RPC
//! v1 transports — `transport-shmem`, `transport-uds`,
//! and `transport-tcp-mux v2` (wire-v2 frames over raw
//! TCP). A fourth thread runs the four RPC kinds
//! against each transport in turn.
//!
//! Gate: all 12 RPCs (4 kinds × 3 transports) succeed
//! and return identical byte sequences regardless of
//! transport. This proves the `Service::dispatch_with_ctx`
//! contract is transport-independent and the wire-v2
//! codec interoperates byte-for-byte across all three
//! pipelines.

#![cfg(all(feature = "transport-shmem", feature = "transport-tcp-mux"))]

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{
    CallContext, Metadata, RpcError, Server, Service, ShmemConnection, Status, StatusCode,
    TcpMuxV2Connection, UdsConnection,
};

// ============================================================
// One service — same logic across all three transports.
// ============================================================

struct Greeter;

impl Service for Greeter {
    fn name(&self) -> &str {
        "ts_e2e.Echo"
    }

    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        _request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "use dispatch_with_ctx".into(),
        }))
    }

    fn dispatch_with_ctx(
        &self,
        method: &str,
        _ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        match method {
            "echo_unary" => Ok(vec![requests.first().cloned().unwrap_or_default()]),
            "fanout_3" => {
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
            "concat" => {
                let mut out = Vec::new();
                for r in requests {
                    if !out.is_empty() {
                        out.push(b'+');
                    }
                    out.extend_from_slice(r);
                }
                Ok(vec![out])
            }
            "reply" => Ok(requests
                .iter()
                .map(|r| {
                    let mut out = b"re:".to_vec();
                    out.extend_from_slice(r);
                    out
                })
                .collect()),
            other => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("unknown method {other}"),
            })),
        }
    }
}

// ============================================================
// Address helpers — fresh paths / ports per run.
// ============================================================

fn tmp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "grpcx-three-{}-{}-{}.sock",
        tag,
        std::process::id(),
        tag.len()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn ephemeral_loopback() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    format!("127.0.0.1:{port}")
}

// ============================================================
// Per-transport client driver — every transport answers
// the same 4 RPC shapes the same way.
// ============================================================

trait Client {
    fn unary(&mut self, body: &[u8]) -> Vec<u8>;
    fn server_stream(&mut self, body: &[u8]) -> Vec<Vec<u8>>;
    fn client_stream(&mut self, bodies: &[&[u8]]) -> Vec<u8>;
    fn bidi(&mut self, bodies: &[&[u8]]) -> Vec<Vec<u8>>;
}

impl Client for UdsConnection {
    fn unary(&mut self, body: &[u8]) -> Vec<u8> {
        let (b, _) = UdsConnection::call_unary(
            self,
            "ts_e2e.Echo",
            "echo_unary",
            &Metadata::default(),
            body,
        )
        .expect("uds unary");
        b
    }
    fn server_stream(&mut self, body: &[u8]) -> Vec<Vec<u8>> {
        let (bs, _) = UdsConnection::call_server_streaming(
            self,
            "ts_e2e.Echo",
            "fanout_3",
            &Metadata::default(),
            body,
        )
        .expect("uds server_stream");
        bs
    }
    fn client_stream(&mut self, bodies: &[&[u8]]) -> Vec<u8> {
        let (b, _) = UdsConnection::call_client_streaming(
            self,
            "ts_e2e.Echo",
            "concat",
            &Metadata::default(),
            bodies,
        )
        .expect("uds client_stream");
        b
    }
    fn bidi(&mut self, bodies: &[&[u8]]) -> Vec<Vec<u8>> {
        let (bs, _) =
            UdsConnection::call_bidi(self, "ts_e2e.Echo", "reply", &Metadata::default(), bodies)
                .expect("uds bidi");
        bs
    }
}

impl Client for ShmemConnection {
    fn unary(&mut self, body: &[u8]) -> Vec<u8> {
        let (b, _) = ShmemConnection::call_unary(
            self,
            "ts_e2e.Echo",
            "echo_unary",
            &Metadata::default(),
            body,
        )
        .expect("shmem unary");
        b
    }
    fn server_stream(&mut self, body: &[u8]) -> Vec<Vec<u8>> {
        let (bs, _) = ShmemConnection::call_server_streaming(
            self,
            "ts_e2e.Echo",
            "fanout_3",
            &Metadata::default(),
            body,
        )
        .expect("shmem server_stream");
        bs
    }
    fn client_stream(&mut self, bodies: &[&[u8]]) -> Vec<u8> {
        let (b, _) = ShmemConnection::call_client_streaming(
            self,
            "ts_e2e.Echo",
            "concat",
            &Metadata::default(),
            bodies,
        )
        .expect("shmem client_stream");
        b
    }
    fn bidi(&mut self, bodies: &[&[u8]]) -> Vec<Vec<u8>> {
        let (bs, _) =
            ShmemConnection::call_bidi(self, "ts_e2e.Echo", "reply", &Metadata::default(), bodies)
                .expect("shmem bidi");
        bs
    }
}

impl Client for TcpMuxV2Connection {
    fn unary(&mut self, body: &[u8]) -> Vec<u8> {
        let (b, _) = TcpMuxV2Connection::call_unary(
            self,
            "ts_e2e.Echo",
            "echo_unary",
            &Metadata::default(),
            body,
        )
        .expect("tcp-mux-v2 unary");
        b
    }
    fn server_stream(&mut self, body: &[u8]) -> Vec<Vec<u8>> {
        let (bs, _) = TcpMuxV2Connection::call_server_streaming(
            self,
            "ts_e2e.Echo",
            "fanout_3",
            &Metadata::default(),
            body,
        )
        .expect("tcp-mux-v2 server_stream");
        bs
    }
    fn client_stream(&mut self, bodies: &[&[u8]]) -> Vec<u8> {
        let (b, _) = TcpMuxV2Connection::call_client_streaming(
            self,
            "ts_e2e.Echo",
            "concat",
            &Metadata::default(),
            bodies,
        )
        .expect("tcp-mux-v2 client_stream");
        b
    }
    fn bidi(&mut self, bodies: &[&[u8]]) -> Vec<Vec<u8>> {
        let (bs, _) = TcpMuxV2Connection::call_bidi(
            self,
            "ts_e2e.Echo",
            "reply",
            &Metadata::default(),
            bodies,
        )
        .expect("tcp-mux-v2 bidi");
        bs
    }
}

fn run_four_kinds<C: Client>(client: &mut C, label: &str) {
    // 1) Unary
    let b = client.unary(b"hello");
    assert_eq!(b, b"hello", "{label}: unary echo");

    // 2) Server streaming — 3 responses
    let bs = client.server_stream(b"fan");
    let strs: Vec<_> = bs
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(
        strs,
        ["fan#0", "fan#1", "fan#2"],
        "{label}: server stream fanout"
    );

    // 3) Client streaming — 3 → 1
    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let b = client.client_stream(&reqs);
    assert_eq!(b, b"a+b+c", "{label}: client stream concat");

    // 4) Bidi — 3 → 3
    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let bs = client.bidi(&reqs);
    let strs: Vec<_> = bs
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(strs, ["re:a", "re:b", "re:c"], "{label}: bidi reply");
}

// ============================================================
// The test — one Server, three serve threads, twelve RPCs.
// ============================================================

#[test]
fn one_service_served_on_all_three_transports() {
    let uds_path = tmp_path("uds");
    let shmem_path = tmp_path("shmem");
    let tcp_addr = ephemeral_loopback();

    // One service registry, three serve loops.
    let mut server = Server::new();
    server.register(Box::new(Greeter));

    let stop = Arc::new(AtomicBool::new(false));

    // Thread 1 — UDS.
    let uds_path_clone = uds_path.clone();
    let server_clone = server.clone();
    let stop_clone = stop.clone();
    let uds_handle = std::thread::spawn(move || {
        let _ = server_clone.listen_uds_until(&uds_path_clone, &stop_clone);
    });

    // Thread 2 — shmem.
    let shmem_path_clone = shmem_path.clone();
    let server_clone = server.clone();
    let stop_clone = stop.clone();
    let shmem_handle = std::thread::spawn(move || {
        let _ = server_clone.listen_shmem_until(&shmem_path_clone, &stop_clone);
    });

    // Thread 3 — tcp-mux v2.
    let tcp_addr_clone = tcp_addr.clone();
    let server_clone = server.clone();
    let stop_clone = stop.clone();
    let tcp_handle = std::thread::spawn(move || {
        let _ = server_clone.listen_tcp_mux_v2_until(&tcp_addr_clone, &stop_clone);
    });

    // Connect-with-retry helpers — each listen_* binds
    // off-thread, so the first client attempts can race
    // it.
    let mut uds = (|| {
        for _ in 0..200 {
            if let Ok(c) = UdsConnection::connect(&uds_path, "localhost") {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("uds server never came up at {uds_path:?}")
    })();
    let mut shmem = (|| {
        for _ in 0..200 {
            if let Ok(c) = ShmemConnection::connect(&shmem_path, "localhost") {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("shmem server never came up at {shmem_path:?}")
    })();
    let mut tcp = (|| {
        for _ in 0..200 {
            if let Ok(c) = TcpMuxV2Connection::connect(&tcp_addr, "localhost") {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("tcp-mux-v2 server never came up at {tcp_addr}")
    })();

    // 4 RPC kinds × 3 transports = 12 RPCs, identical
    // expected outputs.
    run_four_kinds(&mut uds, "uds");
    run_four_kinds(&mut shmem, "shmem");
    run_four_kinds(&mut tcp, "tcp-mux-v2");

    // Tear-down.
    stop.store(true, Ordering::Relaxed);
    let _ = uds_handle.join();
    let _ = shmem_handle.join();
    let _ = tcp_handle.join();
    let _ = std::fs::remove_file(&uds_path);
    let _ = std::fs::remove_file(&shmem_path);
}
