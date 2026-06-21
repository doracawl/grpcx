//! Codegen + `transport-uds` end-to-end —
//! `grpcx-pb-derive::proto_inline!` generates a
//! `RpcChannel`-generic `GreeterClient` and a `Service`-
//! impl `GreeterServer` that overrides
//! `dispatch_with_ctx`; this test drives both halves on
//! the v2 UDS path and asserts:
//!
//! - All four RPC kinds round-trip through the
//!   generated stubs.
//! - The server-side handler's `&mut CallContext`
//!   carries the kernel-attested peer credentials
//!   (proves the codegen `dispatch_with_ctx` override
//!   threads `ctx` correctly).
//! - The generated client compiles against `UdsConnection`
//!   (which implements `RpcChannel`).

#![cfg(feature = "transport-uds")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{CallContext, Server, Status, StatusCode, UdsConnection};

grpcx_pb_derive::proto_inline!(
    r#"syntax = "proto3";
package uds_codegen.v1;

message HelloReq  { string name = 1; }
message HelloResp { string greeting = 1; uint32 uid = 2; }

service Greeter {
  rpc SayHello (HelloReq) returns (HelloResp);
  rpc Fanout   (HelloReq) returns (stream HelloResp);
  rpc Collect  (stream HelloReq) returns (HelloResp);
  rpc Chat     (stream HelloReq) returns (stream HelloResp);
}
"#
);

use uds_codegen::v1::{GreeterClient, GreeterHandler, GreeterServer, HelloReq, HelloResp};

struct MyGreeter;

impl GreeterHandler for MyGreeter {
    fn say_hello(&self, req: HelloReq, ctx: &mut CallContext) -> Result<HelloResp, Status> {
        // peer_cred MUST surface through the codegen
        // `dispatch_with_ctx` override.
        let uid = ctx
            .peer_cred()
            .ok_or_else(|| Status {
                code: StatusCode::FailedPrecondition,
                message: "peer_cred missing in handler ctx".to_string(),
            })?
            .uid;
        ctx.add_trailing("x-served-by", b"codegen");
        Ok(HelloResp {
            greeting: format!("hello, {}", req.name),
            uid,
        })
    }

    fn fanout(&self, req: HelloReq, _ctx: &mut CallContext) -> Result<Vec<HelloResp>, Status> {
        Ok((0..3)
            .map(|i| HelloResp {
                greeting: format!("{}#{i}", req.name),
                uid: 0,
            })
            .collect())
    }

    fn collect(&self, reqs: Vec<HelloReq>, _ctx: &mut CallContext) -> Result<HelloResp, Status> {
        Ok(HelloResp {
            greeting: reqs
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join("+"),
            uid: 0,
        })
    }

    fn chat(&self, reqs: Vec<HelloReq>, _ctx: &mut CallContext) -> Result<Vec<HelloResp>, Status> {
        Ok(reqs
            .into_iter()
            .map(|r| HelloResp {
                greeting: format!("re: {}", r.name),
                uid: 0,
            })
            .collect())
    }
}

unsafe extern "C" {
    fn geteuid() -> u32;
}

fn unique_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "grpcx-uds-codegen-{}-{tag}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn spawn_server(path: std::path::PathBuf) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let h = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        let _ = server.listen_uds_until(&path, &stop_clone);
    });
    (stop, h)
}

fn connect_with_retry(path: &std::path::Path) -> UdsConnection {
    for _ in 0..200 {
        if let Ok(c) = UdsConnection::connect(path, "localhost") {
            return c;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("uds server never came up")
}

#[test]
fn codegen_unary_over_uds_with_peer_cred() {
    let path = unique_path("unary");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let mut ctx = CallContext::default();
    // SAFETY: `geteuid()` is a parameterless POSIX
    // syscall; no pointer arguments.
    let my_uid = unsafe { geteuid() };

    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut ctx,
        &HelloReq {
            name: "world".to_string(),
        },
    )
    .expect("say_hello");
    assert_eq!(resp.greeting, "hello, world");
    assert_eq!(resp.uid, my_uid, "handler-observed peer_cred.uid mismatch");
    assert!(
        ctx.trailing()
            .headers
            .iter()
            .any(|(n, v)| n == "x-served-by" && v == b"codegen")
    );

    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn codegen_server_streaming_over_uds() {
    let path = unique_path("server_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let msgs = GreeterClient::fanout(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "fan".to_string(),
        },
    )
    .expect("fanout");
    let greetings: Vec<_> = msgs.iter().map(|m| m.greeting.as_str()).collect();
    assert_eq!(greetings, ["fan#0", "fan#1", "fan#2"]);

    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn codegen_client_streaming_over_uds() {
    let path = unique_path("client_stream");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let reqs: Vec<HelloReq> = ["a", "b", "c"]
        .iter()
        .map(|n| HelloReq {
            name: n.to_string(),
        })
        .collect();
    let resp =
        GreeterClient::collect(&mut conn, &mut CallContext::default(), &reqs).expect("collect");
    assert_eq!(resp.greeting, "a+b+c");

    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn codegen_bidi_over_uds() {
    let path = unique_path("bidi");
    let (stop, h) = spawn_server(path.clone());
    let mut conn = connect_with_retry(&path);

    let reqs: Vec<HelloReq> = ["a", "b", "c"]
        .iter()
        .map(|n| HelloReq {
            name: n.to_string(),
        })
        .collect();
    let msgs = GreeterClient::chat(&mut conn, &mut CallContext::default(), &reqs).expect("chat");
    let greetings: Vec<_> = msgs.iter().map(|m| m.greeting.as_str()).collect();
    assert_eq!(greetings, ["re: a", "re: b", "re: c"]);

    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    let _ = std::fs::remove_file(&path);
}
