//! Same-host internal RPC over `grpcx/tcp-mux` (loopback).
//!
//! Copy-paste this file as the starting point for an
//! in-house service: one `.proto` inline, one handler,
//! one server thread, one client. No TLS, no HTTP, no
//! handshake on the wire — the lightest of grpcx's four
//! transports, picked because every byte and µs the TLS
//! / HTTP-framing tax would cost is wasted when both
//! ends live on the same kernel.
//!
//! ## Run
//!
//! ```text
//! cargo run --release --example internal_rpc \
//!     --no-default-features --features transport-tcp-mux
//! ```
//!
//! `--no-default-features` keeps the binary off the H/3 +
//! TLS + WS pull-ins; same-host deployments don't need
//! them. The release profile matters — grpcx prioritises
//! perf, and the debug profile leaves a lot on the table.
//!
//! ## What it shows
//!
//! 1. **Schema** — `.proto` inlined via `proto_inline!`
//!    (or `proto!("path/to.proto")` for an out-of-line
//!    file). Codegen lands typed `*Client` / `*Handler` /
//!    `*Server` stubs at compile time.
//! 2. **Server** — `Server::new() + register +
//!    listen_tcp_mux_until` on a worker thread, gated by
//!    an `AtomicBool` so the main thread can shut it down
//!    cleanly.
//! 3. **Client** — `ConnectedChannel::connect_tcp_mux` +
//!    generated `GreeterClient::*` calls. One connection
//!    multiplexes many RPCs (mux stream ids advance per
//!    call) — keep it around, don't reconnect per call.
//! 4. **Metadata + status errors** — `CallContext` carries
//!    request headers in / trailing metadata out;
//!    handler errors come back as `RpcError::Status`.
//! 5. **All four RPC kinds** — unary, server-streaming,
//!    client-streaming, bidi.
//!
//! ## Where this fits in the internal-RPC roadmap
//!
//! This example uses today's `transport-tcp-mux` on
//! loopback — the fastest path that ships in the current
//! release. The full perf-first internal-RPC stack
//! (`transport-shmem` ~1 µs / `transport-uds` ~10 µs /
//! `transport-tcp-mux` + io_uring ~15 µs / TS clients
//! day-1) is the v1 plan in
//! `_dev/RFC-internal-rpc-arch.md`. The same `.proto`
//! and the same generated `*Client` / `*Handler` stubs
//! will carry over — only `ConnectedChannel::connect_*`
//! and `Server::listen_*` change.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use grpcx::{CallContext, ConnectedChannel, RpcError, Server, Status, StatusCode};

grpcx_pb_derive::proto_inline!(
    r#"syntax = "proto3";
package internal.v1;

message HelloReq  { string name = 1; }
message HelloResp { string greeting = 1; }

service Greeter {
  rpc SayHello (HelloReq) returns (HelloResp);
  rpc Fanout   (HelloReq) returns (stream HelloResp);
  rpc Collect  (stream HelloReq) returns (HelloResp);
  rpc Chat     (stream HelloReq) returns (stream HelloResp);
}
"#
);

use internal::v1::{GreeterClient, GreeterHandler, GreeterServer, HelloReq, HelloResp};

struct MyGreeter;

impl GreeterHandler for MyGreeter {
    fn say_hello(&self, req: HelloReq, ctx: &mut CallContext) -> Result<HelloResp, Status> {
        if req.name == "denied" {
            return Err(Status {
                code: StatusCode::PermissionDenied,
                message: "name is on the blocklist".to_string(),
            });
        }
        ctx.add_trailing("x-served-by", b"internal-rpc-example");
        Ok(HelloResp {
            greeting: format!("hello, {}", req.name),
        })
    }

    fn fanout(&self, req: HelloReq, _ctx: &mut CallContext) -> Result<Vec<HelloResp>, Status> {
        Ok((0..3)
            .map(|i| HelloResp {
                greeting: format!("{}#{i}", req.name),
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
        })
    }

    fn chat(&self, reqs: Vec<HelloReq>, _ctx: &mut CallContext) -> Result<Vec<HelloResp>, Status> {
        Ok(reqs
            .into_iter()
            .map(|r| HelloResp {
                greeting: format!("re: {}", r.name),
            })
            .collect())
    }
}

/// Reserve an OS-assigned port without holding the
/// listener — pure convenience for an example that
/// doesn't want a baked-in port.
fn pick_loopback_addr() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let addr = l.local_addr().expect("local_addr").to_string();
    drop(l);
    addr
}

/// Retry-connect — `listen_tcp_mux_until` binds after the
/// server thread starts, so the first attempts can lose
/// the race.
fn connect_with_retry(addr: &str) -> ConnectedChannel {
    for _ in 0..200 {
        if let Ok(c) = ConnectedChannel::connect_tcp_mux(addr, addr) {
            return c;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("tcp-mux server at {addr} never came up");
}

fn main() {
    let addr = pick_loopback_addr();
    let stop = Arc::new(AtomicBool::new(false));

    // -- server thread ------------------------------------------------
    let server_addr = addr.clone();
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        server
            .listen_tcp_mux_until(&server_addr, &server_stop)
            .expect("listen_tcp_mux_until");
    });

    // -- client side --------------------------------------------------
    let mut conn = connect_with_retry(&addr);

    // 1) Unary, with request header in + trailing out.
    let mut ctx = CallContext::default();
    ctx.add_request_header("x-request-id", b"42");
    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut ctx,
        &HelloReq {
            name: "world".into(),
        },
    )
    .expect("say_hello");
    println!("unary       : {}", resp.greeting);
    let served_by = ctx
        .trailing()
        .headers
        .iter()
        .find(|(n, _)| n == "x-served-by")
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    println!("  trailing  : x-served-by = {served_by}");

    // 2) Reuse the same connection — stream ids advance,
    //    TCP socket stays open.
    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "again".into(),
        },
    )
    .expect("say_hello#2");
    println!("unary#2     : {}", resp.greeting);

    // 3) Server-streaming.
    let msgs = GreeterClient::fanout(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq { name: "fan".into() },
    )
    .expect("fanout");
    println!(
        "server-strm : {:?}",
        msgs.iter().map(|m| m.greeting.as_str()).collect::<Vec<_>>()
    );

    // 4) Client-streaming.
    let reqs: Vec<HelloReq> = ["a", "b", "c"]
        .iter()
        .map(|n| HelloReq {
            name: n.to_string(),
        })
        .collect();
    let resp =
        GreeterClient::collect(&mut conn, &mut CallContext::default(), &reqs).expect("collect");
    println!("client-strm : {}", resp.greeting);

    // 5) Bidi.
    let msgs = GreeterClient::chat(&mut conn, &mut CallContext::default(), &reqs).expect("chat");
    println!(
        "bidi        : {:?}",
        msgs.iter().map(|m| m.greeting.as_str()).collect::<Vec<_>>()
    );

    // 6) Status-error path — the handler returned
    //    `PermissionDenied`; the client sees an
    //    `RpcError::Status` with the same code.
    let err = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "denied".into(),
        },
    )
    .expect_err("denied should fail");
    match err {
        RpcError::Status(s) => {
            println!("status-err  : {:?} / {}", s.code, s.message);
            assert_eq!(s.code, StatusCode::PermissionDenied);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // -- shutdown ----------------------------------------------------
    stop.store(true, Ordering::SeqCst);
    let _ = server_thread.join();
}
