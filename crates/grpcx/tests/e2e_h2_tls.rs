//! T4 acceptance — gRPC over h2-with-TLS end to end:
//! `.proto` source → typed stubs → real TLS 1.3 over
//! loopback TCP, `listen_h2` / `connect_h2`. Same H/2
//! session as the h2c path; L3's byte-stream TLS rides as
//! transport middleware with a lazy (non-blocking)
//! handshake on both sides.

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
}

#[test]
fn typed_stubs_round_trip_over_h2_tls_loopback() {
    const SERVER_NAME: &str = "loopback.example";
    let params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h2"]);
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h2"]);

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        server
            .listen_h2_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    // Connect with retry — the server thread binds after
    // spawn.
    let mut conn = None;
    for _ in 0..200 {
        match grpcx::ConnectedChannel::connect_h2(&addr, &client_cfg, SERVER_NAME, &addr) {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let mut conn = conn.expect("h2-tls server never came up");

    // Unary through the real TLS 1.3 handshake.
    let mut ctx = CallContext::default();
    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut ctx,
        &HelloReq {
            name: "加密".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resp.greeting, "hello, 加密");
    assert!(
        ctx.trailing()
            .headers
            .iter()
            .any(|(n, v)| n == "x-served-by" && v == b"my-greeter")
    );

    // Server-streaming on the same TLS session.
    let msgs = GreeterClient::fanout(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "tls".to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.greeting.as_str()).collect::<Vec<_>>(),
        ["tls#0", "tls#1", "tls#2"]
    );

    // Status error path: trailers-only over TLS.
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

/// A client that doesn't trust the server's cert must fail
/// the handshake — no silent cleartext fallback.
#[test]
fn untrusted_cert_fails_the_call() {
    const SERVER_NAME: &str = "loopback.example";
    let params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h2"]);
    // Client with an EMPTY trust store.
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.set_alpn(&[b"h2"]);

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        // The server sees a TLS-alert'd connection die;
        // the loop drops it as a dead peer.
        server
            .listen_h2_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    let mut conn = None;
    for _ in 0..200 {
        match grpcx::ConnectedChannel::connect_h2(&addr, &client_cfg, SERVER_NAME, &addr) {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let mut conn = conn.expect("tcp connect itself should succeed");

    let err = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "x".to_string(),
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, RpcError::Transport(_)),
        "expected a transport (TLS) failure, got {err:?}"
    );

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
