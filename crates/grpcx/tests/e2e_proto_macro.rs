//! D3 acceptance — the full tonic-shaped workflow with the
//! pains repaired: `.proto` source → `proto_inline!` (no
//! protoc, no build.rs) → typed `GreeterClient` /
//! `GreeterHandler` / `GreeterServer` → real loopback RPC
//! over the H3 stack.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use grpcx::{CallContext, Channel, DatagramTransport, Metadata, RpcError, Server, Status};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

grpcx_pb_derive::proto_inline!(
    r#"syntax = "proto3";
package demo.v1;

message HelloReq { string name = 1; }
message HelloResp { string greeting = 1; }

service Greeter {
  rpc SayHello (HelloReq) returns (HelloResp);
}
"#
);

use demo::v1::{GreeterClient, GreeterHandler, GreeterServer, HelloReq, HelloResp};

// ---- the user-side handler: P2 shape, no wrappers ----

struct MyGreeter;

impl GreeterHandler for MyGreeter {
    fn say_hello(&self, req: HelloReq, ctx: &mut CallContext) -> Result<HelloResp, Status> {
        ctx.add_trailing("x-served-by", b"my-greeter");
        Ok(HelloResp {
            greeting: format!("hello, {}", req.name),
        })
    }
}

// ---- loopback transport (same as e2e_unary_h3) ----

#[derive(Clone)]
struct MemTransport {
    tx: Arc<Mutex<VecDeque<Vec<u8>>>>,
    rx: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl MemTransport {
    fn pair() -> (Self, Self) {
        let a = Arc::new(Mutex::new(VecDeque::new()));
        let b = Arc::new(Mutex::new(VecDeque::new()));
        (
            MemTransport {
                tx: a.clone(),
                rx: b.clone(),
            },
            MemTransport { tx: b, rx: a },
        )
    }
}

impl DatagramTransport for MemTransport {
    fn send(&mut self, d: &[u8]) -> Result<(), RpcError> {
        self.tx.lock().unwrap().push_back(d.to_vec());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(self.rx.lock().unwrap().pop_front())
    }
}

#[test]
fn generated_stubs_round_trip_over_h3_loopback() {
    const SERVER_NAME: &str = "loopback.example";
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);

    let (client_t, mut server_t) = MemTransport::pair();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        // Generated bridge: handler in, ::grpcx::Service out.
        server.register(Box::new(GreeterServer(MyGreeter)));
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        while !server_stop.load(Ordering::Relaxed) {
            let mut progressed = false;
            while let Some(d) = server_t.recv().unwrap() {
                for o in conn.pump(Some(&d)).unwrap() {
                    server_t.send(&o).unwrap();
                }
                progressed = true;
            }
            conn.dispatch(&server).unwrap();
            for o in conn.pump(None).unwrap() {
                server_t.send(&o).unwrap();
                progressed = true;
            }
            if !progressed {
                std::thread::yield_now();
            }
        }
    });

    let channel = Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let mut conn = grpcx::ConnectedChannel::new(channel, Box::new(client_t));
    // The whole tonic-habit surface in one call: typed
    // request in, typed response out; the SAME CallContext
    // shape on both sides — request metadata out, trailing
    // metadata back.
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
    // The handler's ctx.add_trailing made it all the way
    // back through the real stack.
    assert!(
        ctx.trailing()
            .headers
            .iter()
            .any(|(n, v)| n == "x-served-by" && v == b"my-greeter")
    );

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn handler_status_error_propagates_through_generated_bridge() {
    // No transport needed: exercise the generated
    // dispatch bridge directly through the Service trait.
    struct Denier;
    impl GreeterHandler for Denier {
        fn say_hello(&self, _req: HelloReq, _ctx: &mut CallContext) -> Result<HelloResp, Status> {
            Err(Status {
                code: grpcx::StatusCode::PermissionDenied,
                message: "nope".to_string(),
            })
        }
    }
    let svc = GreeterServer(Denier);
    let mut req_bytes = Vec::new();
    grpcx_pb::ProtoCodec::encode(
        &HelloReq {
            name: "x".to_string(),
        },
        &mut req_bytes,
    );
    let err =
        grpcx::Service::dispatch(&svc, "SayHello", &Metadata::default(), &req_bytes).unwrap_err();
    assert!(matches!(
        err,
        RpcError::Status(Status {
            code: grpcx::StatusCode::PermissionDenied,
            ..
        })
    ));
    // Unknown method → Unimplemented via the generated arm.
    let err = grpcx::Service::dispatch(&svc, "Nope", &Metadata::default(), &req_bytes).unwrap_err();
    assert!(matches!(
        err,
        RpcError::Status(Status {
            code: grpcx::StatusCode::Unimplemented,
            ..
        })
    ));
}

// ============================================================
// D5 — the tonic-shaped bootstrap on real sockets:
// `Server::listen_h3(addr)` on one thread,
// `ConnectedChannel::connect_h3(addr)` + typed call on
// another. No hand-driven pumps anywhere in user code.
// ============================================================

#[test]
fn listen_and_connect_on_real_udp_sockets() {
    const SERVER_NAME: &str = "loopback.example";
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);

    // Pick a free port, then serve on it.
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(GreeterServer(MyGreeter)));
        server
            .listen_h3_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    // Client: two lines to a typed call.
    let mut conn = grpcx::ConnectedChannel::connect_h3(&addr, &client_cfg, SERVER_NAME).unwrap();
    let mut ctx = CallContext::default();
    let resp = GreeterClient::say_hello(
        &mut conn,
        &mut ctx,
        &HelloReq {
            name: "socket".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resp.greeting, "hello, socket");
    assert!(
        ctx.trailing()
            .headers
            .iter()
            .any(|(n, _)| n == "x-served-by")
    );

    // Second call on the same connection (session reuse).
    let resp2 = GreeterClient::say_hello(
        &mut conn,
        &mut CallContext::default(),
        &HelloReq {
            name: "again".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resp2.greeting, "hello, again");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

// ============================================================
// Stage B presence kernel (charter 2026-06-13): proto3
// `optional` compiles to Option<T> with EXPLICIT presence
// on the wire — Some(default) is encoded, None is not.
// ============================================================

mod presence {
    grpcx_pb_derive::proto_inline!(
        r#"syntax = "proto3";
package presence.v1;

message Inner { int32 v = 1; }

message Probe {
  optional int32 oi = 1;
  optional string os = 2;
  optional bool ob = 3;
  optional Inner om = 4;
  int32 plain = 5;
}
"#
    );

    use grpcx_pb::ProtoCodec;
    use presence::v1::{Inner, Probe};

    fn round_trip(p: &Probe) -> Probe {
        let mut wire = Vec::new();
        p.encode(&mut wire);
        Probe::decode(&wire).expect("decode")
    }

    #[test]
    fn unset_optionals_stay_off_the_wire() {
        let p = Probe::default();
        let mut wire = Vec::new();
        p.encode(&mut wire);
        // plain (implicit presence, value 0) is still
        // written by the current always-encode shape; the
        // optionals must contribute NOTHING. Tag bytes for
        // fields 1-4 must be absent.
        for tag in [0x08u8, 0x12, 0x18, 0x22] {
            assert!(
                !wire.contains(&tag),
                "unset optional leaked tag {tag:#x} onto the wire: {wire:?}"
            );
        }
        assert_eq!(round_trip(&p), p);
    }

    #[test]
    fn some_default_is_explicit_presence() {
        // THE presence bit: Some(0) / Some("") / Some(false)
        // / Some(Inner::default()) all go ON the wire and
        // come back as Some, distinguishable from None.
        let p = Probe {
            oi: Some(0),
            os: Some(String::new()),
            ob: Some(false),
            om: Some(Inner::default()),
            plain: 0,
        };
        let got = round_trip(&p);
        assert_eq!(got.oi, Some(0));
        assert_eq!(got.os, Some(String::new()));
        assert_eq!(got.ob, Some(false));
        assert_eq!(got.om, Some(Inner::default()));
        let none = round_trip(&Probe::default());
        assert_eq!(none.oi, None);
        assert_eq!(none.os, None);
        assert_eq!(none.ob, None);
        assert_eq!(none.om, None);
    }

    #[test]
    fn set_optionals_round_trip_exact() {
        let p = Probe {
            oi: Some(-42),
            os: Some("presence".to_string()),
            ob: Some(true),
            om: Some(Inner { v: 7 }),
            plain: 9,
        };
        assert_eq!(round_trip(&p), p);
    }
}

// ============================================================
// Stage C proto2 frontend (charter 2026-06-12): proto2
// `optional` rides the same explicit-presence kernel
// (Option<T>); `required` compiles to plain T and is always
// encoded (editions LEGACY_REQUIRED encode shape — missing-
// required decode is NOT enforced, matching prost).
// ============================================================

mod proto2 {
    grpcx_pb_derive::proto_inline!(
        r#"syntax = "proto2";
package legacy.v1;

message Inner { optional int32 v = 1; }

message Probe {
  optional int32 oi = 1;
  optional string os = 2;
  optional Inner om = 3;
  required int32 must = 4;
  repeated int32 tags = 5;
}
"#
    );

    use grpcx_pb::ProtoCodec;
    use legacy::v1::{Inner, Probe};

    fn round_trip(p: &Probe) -> Probe {
        let mut wire = Vec::new();
        p.encode(&mut wire);
        Probe::decode(&wire).expect("decode")
    }

    #[test]
    fn proto2_unset_optionals_stay_off_the_wire() {
        let p = Probe::default();
        let mut wire = Vec::new();
        p.encode(&mut wire);
        // Tag bytes for the optional fields 1-3 must be
        // absent; `must` (field 4, always-encoded) is the
        // only emission for a default Probe.
        for tag in [0x08u8, 0x12, 0x1a] {
            assert!(
                !wire.contains(&tag),
                "unset optional leaked tag {tag:#x} onto the wire: {wire:?}"
            );
        }
        // required always-encodes — tag 0x20 present even
        // at value 0 (LEGACY_REQUIRED encode shape).
        assert!(wire.contains(&0x20), "required field missing: {wire:?}");
        assert_eq!(round_trip(&p), p);
    }

    #[test]
    fn proto2_some_default_is_explicit_presence() {
        let p = Probe {
            oi: Some(0),
            os: Some(String::new()),
            om: Some(Inner::default()),
            must: 0,
            tags: Vec::new(),
        };
        let got = round_trip(&p);
        assert_eq!(got.oi, Some(0));
        assert_eq!(got.os, Some(String::new()));
        assert_eq!(got.om, Some(Inner::default()));
        let none = round_trip(&Probe::default());
        assert_eq!(none.oi, None);
        assert_eq!(none.os, None);
        assert_eq!(none.om, None);
    }

    #[test]
    fn proto2_set_fields_round_trip_exact() {
        let p = Probe {
            oi: Some(-42),
            os: Some("legacy".to_string()),
            om: Some(Inner { v: Some(7) }),
            must: 13,
            tags: vec![1, 2, 3],
        };
        assert_eq!(round_trip(&p), p);
    }
}
