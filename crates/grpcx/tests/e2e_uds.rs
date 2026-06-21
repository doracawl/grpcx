//! End-to-end tests for `transport-uds` — wire format v2
//! over AF_UNIX. Covers the four RPC kinds, peer
//! credential surfacing, application Status errors, and
//! the stale-socket reclaim recipe documented in RFC §
//! 4.3.

#![cfg(feature = "transport-uds")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{CallContext, Metadata, RpcError, Server, Service, Status, StatusCode, UdsConnection};

// ============================================================
// Test service — exposes one method per RPC kind plus a
// few cross-cutting behaviours (Status, peer_cred,
// metadata round-trip).
// ============================================================

struct TestService;

impl Service for TestService {
    fn name(&self) -> &str {
        "uds_test.v1.Echo"
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
            "want_peer_cred" => {
                let pc = ctx.peer_cred().ok_or_else(|| {
                    RpcError::Status(Status {
                        code: StatusCode::FailedPrecondition,
                        message: "peer_cred missing".to_string(),
                    })
                })?;
                // Pack uid as little-endian u32 into the
                // response body so the test can assert
                // against geteuid(2).
                Ok(vec![pc.uid.to_le_bytes().to_vec()])
            }
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
            other => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("method {other} not in TestService"),
            })),
        }
    }
}

// ============================================================
// Test harness
// ============================================================

/// Allocate a unique UDS path per test. The path stays
/// inside `std::env::temp_dir()` and is unlinked before
/// the server binds (to avoid cross-test residue).
fn unique_socket_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "grpcx-uds-test-{}-{}-{}.sock",
        tag,
        std::process::id(),
        // Per-test discriminator — tag is unique per
        // test by construction.
        tag.len()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// Spawn a server bound at `path` and return a stop
/// handle.
fn spawn_server(path: PathBuf) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let h = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(TestService));
        let _ = server.listen_uds_until(&path, &stop_clone);
    });
    (stop, h)
}

/// Connect with retry — the server thread binds after
/// spawn, so the first attempts can race it.
fn connect_with_retry(path: &PathBuf) -> UdsConnection {
    for _ in 0..200 {
        if let Ok(c) = UdsConnection::connect(path, "localhost") {
            return c;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("uds server at {path:?} never came up")
}

/// Clean shutdown — flip stop + join the server thread.
fn shutdown(stop: Arc<AtomicBool>, h: std::thread::JoinHandle<()>, path: &PathBuf) {
    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    let _ = std::fs::remove_file(path);
}

// ============================================================
// 1) Unary round-trip + trailing metadata
// ============================================================

#[test]
fn unary_round_trip_with_trailing_metadata() {
    let path = unique_socket_path("unary");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let (body, trailing) = conn
        .call_unary(
            "uds_test.v1.Echo",
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

    shutdown(stop, h, &path);
}

// ============================================================
// 2) Server-streaming — one request, three responses
// ============================================================

#[test]
fn server_streaming_fan_out_3() {
    let path = unique_socket_path("server_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let (bodies, _trailing) = conn
        .call_server_streaming(
            "uds_test.v1.Echo",
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

    shutdown(stop, h, &path);
}

// ============================================================
// 3) Client-streaming — three requests, one response
// ============================================================

#[test]
fn client_streaming_concat() {
    let path = unique_socket_path("client_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (body, _trailing) = conn
        .call_client_streaming(
            "uds_test.v1.Echo",
            "client_stream_concat",
            &Metadata::default(),
            &reqs,
        )
        .expect("client_streaming call");
    assert_eq!(body, b"a+b+c");

    shutdown(stop, h, &path);
}

// ============================================================
// 4) Bidi-streaming — three in, three out
// ============================================================

#[test]
fn bidi_three_each_way() {
    let path = unique_socket_path("bidi");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let reqs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let (bodies, _trailing) = conn
        .call_bidi(
            "uds_test.v1.Echo",
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

    shutdown(stop, h, &path);
}

// ============================================================
// 5) Peer credentials visible to the handler
// ============================================================

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[test]
fn peer_cred_uid_matches_geteuid() {
    let path = unique_socket_path("peer_cred");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let (body, _trailing) = conn
        .call_unary(
            "uds_test.v1.Echo",
            "want_peer_cred",
            &Metadata::default(),
            &[],
        )
        .expect("peer_cred call");
    assert_eq!(
        body.len(),
        4,
        "expected u32 LE uid in response body, got {body:?}"
    );
    let observed_uid = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    // SAFETY: `geteuid` is a simple POSIX syscall; no
    // pointer arguments, no error path beyond the
    // returned uid.
    let my_uid = unsafe { geteuid() };
    assert_eq!(
        observed_uid, my_uid,
        "server-observed peer uid {observed_uid} != caller's geteuid() {my_uid}"
    );

    // Also: the client-side handle exposes the server's
    // peer_cred (symmetric SO_PEERCRED).
    let server_cred = conn.peer_cred().expect("server peer_cred should be Some");
    assert_eq!(server_cred.uid, my_uid);

    shutdown(stop, h, &path);
}

// ============================================================
// 6) Application Status — PermissionDenied surfaces as
//    RpcError::Status to the client
// ============================================================

#[test]
fn status_error_round_trip() {
    let path = unique_socket_path("denied");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let err = conn
        .call_unary("uds_test.v1.Echo", "denied", &Metadata::default(), b"x")
        .expect_err("denied should fail");
    match err {
        RpcError::Status(s) => {
            assert_eq!(s.code, StatusCode::PermissionDenied);
            assert_eq!(s.message, "blocked by test");
        }
        other => panic!("expected RpcError::Status, got {other:?}"),
    }

    shutdown(stop, h, &path);
}

// ============================================================
// 7) Request header round-trip
// ============================================================

#[test]
fn request_header_visible_to_handler() {
    let path = unique_socket_path("req_header");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let mut meta = Metadata::default();
    meta.headers
        .push(("x-request-id".to_string(), b"abc-123".to_vec()));
    let (body, _trailing) = conn
        .call_unary("uds_test.v1.Echo", "echo_request_header", &meta, &[])
        .expect("call");
    assert_eq!(body, b"abc-123");

    shutdown(stop, h, &path);
}

// ============================================================
// 8) Stale socket reclaim recipe (RFC § 4.3)
// ============================================================

#[test]
fn bind_fails_on_residual_socket_then_reclaim_works() {
    let path = unique_socket_path("reclaim");

    // Round 1 — start a server, then drop it without
    // unlinking the socket (mimics a crash).
    let listener_round_1 = std::os::unix::net::UnixListener::bind(&path).expect("round 1 bind");
    drop(listener_round_1);
    assert!(path.exists(), "socket file should persist after drop");

    // Round 2 — `listen_uds` MUST fail on the residual
    // file per RFC § 4.3.
    let server = Server::new();
    let stop = AtomicBool::new(false);
    let err = server
        .listen_uds_until(&path, &stop)
        .expect_err("listen_uds should refuse to bind on a stale socket");
    match err {
        RpcError::Transport(_) => { /* expected */ }
        other => panic!("expected Transport error, got {other:?}"),
    }

    // Round 3 — the documented reclaim recipe:
    //   probe-then-unlink, then bind succeeds.
    safe_reclaim(&path).expect("reclaim recipe");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);
    let (body, _) = conn
        .call_unary(
            "uds_test.v1.Echo",
            "unary_echo",
            &Metadata::default(),
            b"reclaimed",
        )
        .expect("post-reclaim call");
    assert_eq!(body, b"reclaimed");
    shutdown(stop, h, &path);
}

/// The reclaim recipe documented in RFC § 4.3 — probe
/// the path with a connect; if connection fails the
/// file is dead and safe to unlink. Reproduced verbatim
/// in the test so this asserts the documented snippet
/// keeps compiling.
fn safe_reclaim(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    if path.exists()
        && path.symlink_metadata()?.file_type().is_socket()
        && std::os::unix::net::UnixStream::connect(path).is_err()
    {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
