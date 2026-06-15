//! T5a acceptance — the h2c interop bridge end to end:
//! `.proto` source → `proto_inline!` typed stubs → real
//! gRPC-over-cleartext-HTTP/2 on loopback TCP sockets.
//! Same user surface as the H/3 path (`listen_h2c` /
//! `connect_h2c`), zero TLS setup.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grpcx::{CallContext, RpcError, Server, Status, StatusCode};

grpcx_pb_derive::proto_inline!(
    r#"syntax = "proto3";
package demo.v1;

message HelloReq { string name = 1; }
message HelloResp { string greeting = 1; }

service Greeter {
  rpc SayHello (HelloReq) returns (HelloResp);
  rpc Fanout (HelloReq) returns (stream HelloResp);
  rpc Collect (stream HelloReq) returns (HelloResp);
  rpc Chat (stream HelloReq) returns (stream HelloResp);
}
"#
);

use demo::v1::{GreeterClient, GreeterHandler, GreeterServer, HelloReq, HelloResp};

struct MyGreeter;

impl GreeterHandler for MyGreeter {
    fn say_hello(&self, req: HelloReq, ctx: &mut CallContext) -> Result<HelloResp, Status> {
        if req.name == "denied" {
            return Err(Status {
                code: StatusCode::PermissionDenied,
                message: "nope".to_string(),
            });
        }
        ctx.add_trailing("x-served-by", b"my-greeter");
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

/// TCP connect with retry — the server thread binds after
/// spawn, so the first attempts can race it.
fn connect_with_retry(addr: &str) -> grpcx::ConnectedChannel {
    for _ in 0..200 {
        if let Ok(c) = grpcx::ConnectedChannel::connect_h2c(addr, addr) {
            return c;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("h2c server at {addr} never came up");
}

#[test]
fn typed_stubs_round_trip_over_h2c_tcp_loopback() {
    // Pick a free port, then serve on it (same probe
    // pattern as the UDP e2e).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    // Client: two lines to a typed call — no TLS, no
    // certs, no protoc, no build.rs.
    let mut conn = connect_with_retry(&addr);
    let mut ctx = CallContext::default();
    ctx.add_request_header("x-request-id", b"42");
    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut ctx,
        &HelloReq {
            name: "世界".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resp.greeting, "hello, 世界");
    // Trailing metadata made it back through the real h2c
    // stack (DATA-then-trailers ordering on the wire).
    assert!(
        ctx.trailing()
            .headers
            .iter()
            .any(|(n, v)| n == "x-served-by" && v == b"my-greeter")
    );

    // Second call on the same connection (h2 stream id
    // advance + soft-close reap path).
    let resp2 = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "again".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resp2.greeting, "hello, again");

    // Server-streaming: one request, three responses.
    let msgs = GreeterClient::fanout(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "fan".to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.greeting.as_str()).collect::<Vec<_>>(),
        ["fan#0", "fan#1", "fan#2"]
    );

    // Client-streaming: three requests, one response.
    let reqs: Vec<HelloReq> = ["a", "b", "c"]
        .iter()
        .map(|n| HelloReq {
            name: n.to_string(),
        })
        .collect();
    let resp = GreeterClient::collect(&mut conn, &mut CallContext::default(), &reqs).unwrap();
    assert_eq!(resp.greeting, "a+b+c");

    // Bidi: N in, N out.
    let msgs = GreeterClient::chat(&mut conn, &mut CallContext::default(), &reqs).unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.greeting.as_str()).collect::<Vec<_>>(),
        ["re: a", "re: b", "re: c"]
    );

    // Status error path: trailers-only response.
    let err = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "denied".to_string(),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        RpcError::Status(Status {
            code: StatusCode::PermissionDenied,
            ..
        })
    ));

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
