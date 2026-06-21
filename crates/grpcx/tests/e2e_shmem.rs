//! End-to-end tests for `transport-shmem` — wire format
//! v2 over a shared-memory SPSC ring pair with UDS
//! rendezvous + wake fds via SCM_RIGHTS.
//!
//! Covers the four RPC kinds + Status error round trip,
//! both with and without the busy-poll opt-in.

#![cfg(feature = "transport-shmem")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{
    CallContext, Metadata, RpcError, Server, Service, ShmemConnection, Status, StatusCode,
};

struct TestService;

impl Service for TestService {
    fn name(&self) -> &str {
        "shmem_test.v1.Echo"
    }

    fn dispatch(
        &self,
        _method: &str,
        _meta: &Metadata,
        _request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "use dispatch_with_ctx".to_string(),
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
                ctx.add_trailing("x-handler", b"shmem-echo");
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
                message: "blocked".to_string(),
            })),
            other => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("method {other} not implemented"),
            })),
        }
    }
}

fn unique_path(tag: &str) -> PathBuf {
    let p = PathBuf::from(format!(
        "/tmp/grpcx-shmem-e2e-{}-{tag}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn spawn_server(path: PathBuf) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let h = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(TestService));
        let _ = server.listen_shmem_until(&path, &stop_clone);
    });
    (stop, h)
}

fn spawn_server_busy_poll(path: PathBuf) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let h = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(TestService));
        let _ = server.listen_shmem_busy_poll_until(&path, &stop_clone);
    });
    (stop, h)
}

fn connect_with_retry(path: &PathBuf) -> ShmemConnection {
    std::thread::sleep(Duration::from_millis(20));
    for _ in 0..200 {
        if path.exists()
            && let Ok(c) = ShmemConnection::connect(path, "localhost")
        {
            return c;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("shmem server at {path:?} never came up")
}

fn shutdown(stop: Arc<AtomicBool>, h: std::thread::JoinHandle<()>, path: &PathBuf) {
    stop.store(true, Ordering::Relaxed);
    // listen_shmem_until polls stop every ~2ms.
    let _ = h.join();
    let _ = std::fs::remove_file(path);
}

#[test]
fn unary_round_trip_with_trailing_metadata() {
    let path = unique_path("unary");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let (body, trailing) = conn
        .call_unary(
            "shmem_test.v1.Echo",
            "unary_echo",
            &Metadata::default(),
            b"hello",
        )
        .expect("unary");
    assert_eq!(body, b"hello");
    assert!(
        trailing
            .headers
            .iter()
            .any(|(n, v)| n == "x-handler" && v == b"shmem-echo")
    );
    shutdown(stop, h, &path);
}

#[test]
fn server_streaming_fan_out() {
    let path = unique_path("server_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let (msgs, _trailing) = conn
        .call_server_streaming(
            "shmem_test.v1.Echo",
            "server_stream_fanout",
            &Metadata::default(),
            b"fan",
        )
        .expect("server_streaming");
    let strs: Vec<_> = msgs
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(strs, ["fan#0", "fan#1", "fan#2"]);
    shutdown(stop, h, &path);
}

#[test]
fn client_streaming_concat() {
    let path = unique_path("client_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (body, _trailing) = conn
        .call_client_streaming(
            "shmem_test.v1.Echo",
            "client_stream_concat",
            &Metadata::default(),
            &reqs,
        )
        .expect("client_streaming");
    assert_eq!(body, b"a+b+c");
    shutdown(stop, h, &path);
}

#[test]
fn bidi_three_each_way() {
    let path = unique_path("bidi");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (msgs, _trailing) = conn
        .call_bidi(
            "shmem_test.v1.Echo",
            "bidi_reply",
            &Metadata::default(),
            &reqs,
        )
        .expect("bidi");
    let strs: Vec<_> = msgs
        .iter()
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect();
    assert_eq!(strs, ["re:a", "re:b", "re:c"]);
    shutdown(stop, h, &path);
}

#[test]
fn status_error_round_trip() {
    let path = unique_path("denied");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let err = conn
        .call_unary("shmem_test.v1.Echo", "denied", &Metadata::default(), b"x")
        .expect_err("denied should fail");
    match err {
        RpcError::Status(s) => {
            assert_eq!(s.code, StatusCode::PermissionDenied);
            assert_eq!(s.message, "blocked");
        }
        other => panic!("expected RpcError::Status, got {other:?}"),
    }
    shutdown(stop, h, &path);
}

#[test]
fn busy_poll_on_unary_round_trip() {
    let path = unique_path("busy_poll");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    conn.set_busy_poll_threshold(Duration::from_micros(500));
    let (body, _trailing) = conn
        .call_unary(
            "shmem_test.v1.Echo",
            "unary_echo",
            &Metadata::default(),
            b"busy",
        )
        .expect("unary busy-poll");
    assert_eq!(body, b"busy");
    shutdown(stop, h, &path);
}

#[test]
fn busy_poll_unbounded_round_trip_and_server_stream() {
    // Verify the symmetric `listen_shmem_busy_poll_until`
    // + `set_busy_poll_unbounded(true)` path: both ends
    // drop wake-fd syscalls, the consumer spins on the
    // ring head, and the round-trip still completes
    // correctly. Also exercises server-streaming through
    // the busy-poll-only worker.
    let path = unique_path("busy_poll_unbounded");
    let (stop, h) = spawn_server_busy_poll(path.clone());
    let mut conn = connect_with_retry(&path);
    conn.set_busy_poll_unbounded(true);

    let (body, _trailing) = conn
        .call_unary(
            "shmem_test.v1.Echo",
            "unary_echo",
            &Metadata::default(),
            b"unbounded",
        )
        .expect("unary unbounded busy-poll");
    assert_eq!(body, b"unbounded");

    let (bodies, _trailing) = conn
        .call_server_streaming(
            "shmem_test.v1.Echo",
            "server_stream_fanout",
            &Metadata::default(),
            b"seed",
        )
        .expect("server stream unbounded busy-poll");
    assert_eq!(bodies.len(), 3);
    for (i, b) in bodies.iter().enumerate() {
        let mut expect = b"seed#".to_vec();
        expect.push(b'0' + i as u8);
        assert_eq!(b, &expect);
    }

    shutdown(stop, h, &path);
}
