//! H2 interop bridge GATE (charter 2026-06-11): live
//! bidirectional gRPC interop between grpcx (h2c transport)
//! and tonic, over real TCP loopback.
//!
//! Direction A: grpcx client  → tonic server
//! Direction B: tonic client  → grpcx server
//!
//! Exits 0 with "INTEROP OK" only if both directions echo
//! correctly across payload sizes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub mod echo {
    tonic::include_proto!("echo.v1");
}

pub mod probe2 {
    tonic::include_proto!("probe2.v1");
}

const PAYLOAD_SIZES: &[usize] = &[16, 1024, 65536];

/// Hand-encode `PingRequest { payload }` (field 1, LEN) —
/// the grpcx lane carries raw message bytes; PingRequest /
/// PingResponse share the wire layout.
fn encode_ping(size: usize) -> Vec<u8> {
    let mut req = Vec::with_capacity(size + 8);
    req.push(0x0A);
    let mut n = size;
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            req.push(b);
            break;
        }
        req.push(b | 0x80);
    }
    req.extend(std::iter::repeat_n(0xAB, size));
    req
}

// ============================================================
// Direction A: grpcx client → tonic server (h2c).
// ============================================================

struct TonicEcho;

#[tonic::async_trait]
impl echo::echo_server::Echo for TonicEcho {
    async fn ping(
        &self,
        request: tonic::Request<echo::PingRequest>,
    ) -> Result<tonic::Response<echo::PingResponse>, tonic::Status> {
        let payload = request.into_inner().payload;
        if payload.starts_with(b"sleep:") {
            // Deadline interop case: a deliberately slow
            // handler (tonic enforces the grpc-timeout
            // header server-side too).
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(tonic::Response::new(echo::PingResponse { payload }))
    }
    // Streaming rpcs exist for the interop gate; the
    // bench lane only exercises Ping.
    type BurstStream = tonic::codegen::BoxStream<echo::PingResponse>;
    #[allow(clippy::result_large_err)] // tonic::Status in the iter closure
    async fn burst(
        &self,
        request: tonic::Request<echo::PingRequest>,
    ) -> Result<tonic::Response<Self::BurstStream>, tonic::Status> {
        let payload = request.into_inner().payload;
        let out: Vec<Result<echo::PingResponse, tonic::Status>> = (0..3)
            .map(|_| {
                Ok(echo::PingResponse {
                    payload: payload.clone(),
                })
            })
            .collect();
        Ok(tonic::Response::new(Box::pin(tokio_stream::iter(out))))
    }

    type ExchangeStream = tonic::codegen::BoxStream<echo::PingResponse>;
    async fn exchange(
        &self,
        request: tonic::Request<tonic::Streaming<echo::PingRequest>>,
    ) -> Result<tonic::Response<Self::ExchangeStream>, tonic::Status> {
        let mut stream = request.into_inner();
        let mut out = Vec::new();
        while let Some(req) = stream.message().await? {
            out.push(Ok(echo::PingResponse {
                payload: req.payload,
            }));
        }
        Ok(tonic::Response::new(Box::pin(tokio_stream::iter(out))))
    }
}

fn grpcx_client_to_tonic_server() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let addr = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(echo::echo_server::EchoServer::new(TonicEcho))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        addr
    });

    // tonic without TLS IS h2c (prior knowledge) — exactly
    // what ConnectedChannel::connect_h2c speaks.
    let mut conn =
        grpcx::ConnectedChannel::connect_h2c(&addr.to_string(), &addr.to_string()).unwrap();
    let method = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Ping".to_string(),
        kind: grpcx::RpcKind::Unary,
    };

    for &size in PAYLOAD_SIZES {
        let req = encode_ping(size);
        for round in 0..3 {
            let (resp, _) = conn
                .call_unary(&method, &grpcx::Metadata::default(), &req)
                .unwrap_or_else(|e| panic!("grpcx→tonic size={size} round={round} failed: {e:?}"));
            // PingResponse mirrors PingRequest byte-for-byte.
            assert_eq!(resp, req, "grpcx→tonic echo mismatch at size={size}");
        }
        println!("grpcx-client → tonic-server: size={size} OK");
    }
    // Server-streaming: tonic's Burst answers three copies.
    let burst = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Burst".to_string(),
        kind: grpcx::RpcKind::ServerStreaming,
    };
    let req = encode_ping(1024);
    let (msgs, _) = conn
        .call_server_streaming(&burst, &grpcx::Metadata::default(), &req)
        .unwrap();
    assert_eq!(msgs.len(), 3, "expected 3 burst responses");
    assert!(msgs.iter().all(|m| m == &req));
    println!("grpcx-client → tonic-server: server-streaming OK (3 msgs)");

    // Bidi: tonic's Exchange echoes every request.
    let exchange = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Exchange".to_string(),
        kind: grpcx::RpcKind::BidirectionalStreaming,
    };
    let reqs = [encode_ping(16), encode_ping(1024), encode_ping(4096)];
    let refs: Vec<&[u8]> = reqs.iter().map(|r| r.as_slice()).collect();
    let (msgs, _) = conn
        .call_bidi(&exchange, &grpcx::Metadata::default(), &refs)
        .unwrap();
    assert_eq!(msgs, reqs.to_vec(), "bidi echo mismatch");
    println!("grpcx-client → tonic-server: bidi OK (3 msgs)");

    // Status propagation: tonic answers unknown methods
    // with a non-Ok gRPC status (trailers / trailers-only),
    // which must surface as RpcError::Status.
    let bad = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Nope".to_string(),
        kind: grpcx::RpcKind::Unary,
    };
    match conn.call_unary(&bad, &grpcx::Metadata::default(), &encode_ping(4)) {
        Err(grpcx::RpcError::Status(s)) => {
            println!(
                "grpcx-client → tonic-server: error status OK ({:?})",
                s.code
            );
        }
        other => panic!("expected Status error from tonic, got {other:?}"),
    }

    // Per-call deadline (charter 2026-06-12): the standard
    // grpc-timeout header against a real gRPC peer — a
    // 500 ms tonic handler under a 100 ms deadline must
    // come back DEADLINE_EXCEEDED (locally enforced; tonic
    // accepts and enforces the header server-side too).
    let mut sleep_req = Vec::new();
    {
        // PingRequest { payload = b"sleep:..." }, protobuf
        // field 1, length-delimited.
        let body = b"sleep:please".to_vec();
        sleep_req.push(0x0a);
        sleep_req.push(body.len() as u8);
        sleep_req.extend_from_slice(&body);
    }
    let deadline_meta = grpcx::Metadata {
        headers: vec![
            grpcx::GrpcTimeout::from_duration(std::time::Duration::from_millis(100)).to_header(),
        ],
    };
    let t0 = std::time::Instant::now();
    match conn.call_unary(&method_for("Ping"), &deadline_meta, &sleep_req) {
        Err(grpcx::RpcError::Status(s)) if s.code == grpcx::StatusCode::DeadlineExceeded => {
            assert!(
                t0.elapsed() < std::time::Duration::from_millis(400),
                "deadline must fire near 100 ms"
            );
            println!("grpcx-client → tonic-server: deadline OK (DEADLINE_EXCEEDED)");
        }
        other => panic!("expected DEADLINE_EXCEEDED from the deadline case, got {other:?}"),
    }
    // The connection survives the deadline verdict.
    let req = encode_ping(16);
    let (resp, _) = conn
        .call_unary(&method_for("Ping"), &grpcx::Metadata::default(), &req)
        .expect("connection must survive a deadline-exceeded call");
    assert_eq!(resp, req);
    println!("grpcx-client → tonic-server: post-deadline reuse OK");
}

// ============================================================
// Stage B presence wire-compat: grpcx-pb codegen vs prost,
// same message, byte-identical + cross-decodable.
// ============================================================

mod presence_probe {
    grpcx_pb_derive::proto_inline!(
        r#"syntax = "proto3";
package echo.v1;

message PresenceProbe {
  optional int32 oi = 1;
  optional string os = 2;
  optional bool ob = 3;
  int32 plain = 4;
}
"#
    );
}

fn presence_wire_compat() {
    use grpcx_pb::ProtoCodec;
    use prost::Message as _;
    type Ours = presence_probe::echo::v1::PresenceProbe;
    type Theirs = echo::PresenceProbe;

    let cases: Vec<(Ours, Theirs)> = vec![
        (Ours::default(), Theirs::default()),
        (
            Ours {
                oi: Some(0),
                os: Some(String::new()),
                ob: Some(false),
                plain: 0,
            },
            Theirs {
                oi: Some(0),
                os: Some(String::new()),
                ob: Some(false),
                plain: 0,
            },
        ),
        (
            Ours {
                oi: Some(-7),
                os: Some("presence".into()),
                ob: Some(true),
                plain: 3,
            },
            Theirs {
                oi: Some(-7),
                os: Some("presence".into()),
                ob: Some(true),
                plain: 3,
            },
        ),
    ];
    for (i, (ours, theirs)) in cases.iter().enumerate() {
        let mut our_wire = Vec::new();
        ours.encode(&mut our_wire);
        let prost_wire = theirs.encode_to_vec();
        // prost skips default-valued IMPLICIT fields
        // (plain = 0); grpcx-pb writes them — both legal
        // proto3, so compare via cross-decode (semantics)
        // and additionally byte-equal when plain != 0.
        let theirs_decoded = Theirs::decode(our_wire.as_slice())
            .unwrap_or_else(|e| panic!("prost failed to decode our wire (case {i}): {e}"));
        assert_eq!(
            (
                &theirs_decoded.oi,
                &theirs_decoded.os,
                &theirs_decoded.ob,
                theirs_decoded.plain
            ),
            (&theirs.oi, &theirs.os, &theirs.ob, theirs.plain),
            "case {i}: prost decoded different semantics from our wire"
        );
        let ours_decoded = Ours::decode(&prost_wire)
            .unwrap_or_else(|e| panic!("we failed to decode prost wire (case {i}): {e:?}"));
        assert_eq!(
            (
                &ours_decoded.oi,
                &ours_decoded.os,
                &ours_decoded.ob,
                ours_decoded.plain
            ),
            (&ours.oi, &ours.os, &ours.ob, ours.plain),
            "case {i}: we decoded different semantics from prost wire"
        );
        if theirs.plain != 0 {
            assert_eq!(
                our_wire, prost_wire,
                "case {i}: wire bytes must match prost when no implicit-default fields differ"
            );
        }
    }
    println!("presence wire-compat vs prost: OK ({} cases)", cases.len());
}

// ============================================================
// Stage C proto2 wire-compat: grpcx-pb codegen vs prost on a
// `syntax = "proto2"` source — optional explicit presence,
// required always-encoded, repeated unpacked (proto2 default).
// ============================================================

mod legacy_probe {
    grpcx_pb_derive::proto_inline!(
        r#"syntax = "proto2";
package probe2.v1;

message LegacyProbe {
  optional int32 oi = 1;
  optional string os = 2;
  optional bool ob = 3;
  required int32 must = 4;
  repeated int32 tags = 5;
}
"#
    );
}

fn proto2_wire_compat() {
    use grpcx_pb::ProtoCodec;
    use prost::Message as _;
    type Ours = legacy_probe::probe2::v1::LegacyProbe;
    type Theirs = probe2::LegacyProbe;

    let cases: Vec<(Ours, Theirs)> = vec![
        (Ours::default(), Theirs::default()),
        (
            Ours {
                oi: Some(0),
                os: Some(String::new()),
                ob: Some(false),
                must: 0,
                tags: vec![],
            },
            Theirs {
                oi: Some(0),
                os: Some(String::new()),
                ob: Some(false),
                must: 0,
                tags: vec![],
            },
        ),
        (
            Ours {
                oi: Some(-7),
                os: Some("legacy".into()),
                ob: Some(true),
                must: 13,
                tags: vec![1, 2, 300],
            },
            Theirs {
                oi: Some(-7),
                os: Some("legacy".into()),
                ob: Some(true),
                must: 13,
                tags: vec![1, 2, 300],
            },
        ),
    ];
    for (i, (ours, theirs)) in cases.iter().enumerate() {
        let mut our_wire = Vec::new();
        ours.encode(&mut our_wire);
        let prost_wire = theirs.encode_to_vec();
        let theirs_decoded = Theirs::decode(our_wire.as_slice())
            .unwrap_or_else(|e| panic!("prost failed to decode our proto2 wire (case {i}): {e}"));
        assert_eq!(
            &theirs_decoded, theirs,
            "case {i}: prost decoded different semantics from our proto2 wire"
        );
        let ours_decoded = Ours::decode(&prost_wire)
            .unwrap_or_else(|e| panic!("we failed to decode prost proto2 wire (case {i}): {e:?}"));
        assert_eq!(
            &ours_decoded, ours,
            "case {i}: we decoded different semantics from prost proto2 wire"
        );
        assert_eq!(
            our_wire, prost_wire,
            "case {i}: proto2 wire bytes must match prost"
        );
    }
    println!("proto2 wire-compat vs prost: OK ({} cases)", cases.len());
}

fn method_for(name: &str) -> grpcx::Method {
    grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: name.to_string(),
        kind: grpcx::RpcKind::Unary,
    }
}

// ============================================================
// Direction B: tonic client → grpcx server (h2c).
// ============================================================

struct GrpcxEcho;

impl grpcx::Service for GrpcxEcho {
    fn name(&self) -> &str {
        "echo.v1.Echo"
    }
    fn dispatch(
        &self,
        _method: &str,
        _metadata: &grpcx::Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, grpcx::Metadata), grpcx::RpcError> {
        Ok((request.to_vec(), grpcx::Metadata::default()))
    }
    fn dispatch_streaming(
        &self,
        method: &str,
        _metadata: &grpcx::Metadata,
        requests: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, grpcx::Metadata), grpcx::RpcError> {
        let responses = match method {
            // 3 copies of the single request.
            "Burst" => (0..3)
                .map(|_| requests.first().cloned().unwrap_or_default())
                .collect(),
            // Echo every request message.
            _ => requests.to_vec(),
        };
        Ok((responses, grpcx::Metadata::default()))
    }
}

fn tonic_client_to_grpcx_server() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = grpcx::Server::new();
        server.register(Box::new(GrpcxEcho));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Retry connect — the server thread binds after spawn.
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{addr}")).unwrap();
        let mut chan = None;
        for _ in 0..200 {
            match endpoint.connect().await {
                Ok(c) => {
                    chan = Some(c);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        let mut client =
            echo::echo_client::EchoClient::new(chan.expect("grpcx h2c server never came up"));

        for &size in PAYLOAD_SIZES {
            let req = echo::PingRequest {
                payload: vec![0xAB; size],
            };
            for round in 0..3 {
                let resp = client
                    .ping(tonic::Request::new(req.clone()))
                    .await
                    .unwrap_or_else(|e| {
                        panic!("tonic→grpcx size={size} round={round} failed: {e:?}")
                    });
                assert_eq!(
                    resp.into_inner().payload,
                    req.payload,
                    "tonic→grpcx echo mismatch at size={size}"
                );
            }
            println!("tonic-client → grpcx-server: size={size} OK");
        }

        // Server-streaming: grpcx's Burst answers 3 copies.
        let resp = client
            .burst(tonic::Request::new(echo::PingRequest {
                payload: vec![0xCD; 1024],
            }))
            .await
            .expect("tonic→grpcx burst failed");
        let mut stream = resp.into_inner();
        let mut got = Vec::new();
        while let Some(msg) = stream.message().await.expect("burst stream errored") {
            got.push(msg.payload);
        }
        assert_eq!(got.len(), 3, "expected 3 burst responses");
        assert!(got.iter().all(|p| p == &vec![0xCD; 1024]));
        println!("tonic-client → grpcx-server: server-streaming OK (3 msgs)");

        // Bidi: grpcx's Exchange echoes every request.
        let reqs: Vec<echo::PingRequest> = [16usize, 1024, 4096]
            .iter()
            .map(|&n| echo::PingRequest {
                payload: vec![0xEF; n],
            })
            .collect();
        let expect: Vec<Vec<u8>> = reqs.iter().map(|r| r.payload.clone()).collect();
        let resp = client
            .exchange(tonic::Request::new(tokio_stream::iter(reqs)))
            .await
            .expect("tonic→grpcx exchange failed");
        let mut stream = resp.into_inner();
        let mut got = Vec::new();
        while let Some(msg) = stream.message().await.expect("exchange stream errored") {
            got.push(msg.payload);
        }
        assert_eq!(got, expect, "bidi echo mismatch");
        println!("tonic-client → grpcx-server: bidi OK (3 msgs)");
    });

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

// ============================================================
// TLS forms (charter 2026-06-12): the h2-over-TLS transport
// proven against real tonic in BOTH directions — named since
// v0.11, when the TLS form was loopback-verified only.
// ============================================================

const TLS_SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>, String, String) {
    let params = rcgen::CertificateParams::new(vec![TLS_SERVER_NAME.to_string()]).unwrap();
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (
        cert.der().to_vec(),
        kp.serialize_der(),
        cert.pem(),
        kp.serialize_pem(),
    )
}

fn grpcx_client_to_tonic_tls_server() {
    let (cert_der, _key_der, cert_pem, key_pem) = mint_cert();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let addr = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let identity = tonic::transport::Identity::from_pem(&cert_pem, &key_pem);
        tokio::spawn(
            tonic::transport::Server::builder()
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
                .unwrap()
                .add_service(echo::echo_server::EchoServer::new(TonicEcho))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        addr
    });

    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[b"h2"]);
    let mut conn = grpcx::ConnectedChannel::connect_h2(
        &addr.to_string(),
        &client_cfg,
        TLS_SERVER_NAME,
        &addr.to_string(),
    )
    .unwrap();
    let method = method_for("Ping");

    for &size in PAYLOAD_SIZES {
        let req = encode_ping(size);
        for round in 0..3 {
            let (resp, _) = conn
                .call_unary(&method, &grpcx::Metadata::default(), &req)
                .unwrap_or_else(|e| {
                    panic!("grpcx→tonic-TLS size={size} round={round} failed: {e:?}")
                });
            assert_eq!(resp, req, "grpcx→tonic-TLS echo mismatch at size={size}");
        }
        println!("grpcx-client → tonic-TLS-server: size={size} OK");
    }
}

fn tonic_client_to_grpcx_tls_server() {
    let (cert_der, key_der, cert_pem, _key_pem) = mint_cert();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[b"h2"]);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = grpcx::Server::new();
        server.register(Box::new(GrpcxEcho));
        server
            .listen_h2_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let tls = tonic::transport::ClientTlsConfig::new()
            .ca_certificate(tonic::transport::Certificate::from_pem(&cert_pem))
            .domain_name(TLS_SERVER_NAME);
        let endpoint = tonic::transport::Endpoint::from_shared(format!("https://{addr}"))
            .unwrap()
            .tls_config(tls)
            .unwrap();
        let mut chan = None;
        for _ in 0..200 {
            match endpoint.connect().await {
                Ok(c) => {
                    chan = Some(c);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        let mut client =
            echo::echo_client::EchoClient::new(chan.expect("grpcx TLS server never came up"));

        for &size in PAYLOAD_SIZES {
            let req = echo::PingRequest {
                payload: vec![0xAB; size],
            };
            for round in 0..3 {
                let resp = client
                    .ping(tonic::Request::new(req.clone()))
                    .await
                    .unwrap_or_else(|e| {
                        panic!("tonic→grpcx-TLS size={size} round={round} failed: {e:?}")
                    });
                assert_eq!(
                    resp.into_inner().payload,
                    req.payload,
                    "tonic→grpcx-TLS echo mismatch at size={size}"
                );
            }
            println!("tonic-TLS-client → grpcx-server: size={size} OK");
        }

        let resp = client
            .burst(tonic::Request::new(echo::PingRequest {
                payload: vec![0xCD; 1024],
            }))
            .await
            .expect("tonic→grpcx-TLS burst failed");
        let mut stream = resp.into_inner();
        let mut got = Vec::new();
        while let Some(msg) = stream.message().await.expect("burst stream errored") {
            got.push(msg.payload);
        }
        assert_eq!(got.len(), 3, "expected 3 burst responses");
        assert!(got.iter().all(|p| p == &vec![0xCD; 1024]));
        println!("tonic-TLS-client → grpcx-server: server-streaming OK (3 msgs)");

        let reqs: Vec<echo::PingRequest> = [16usize, 1024, 4096]
            .iter()
            .map(|&n| echo::PingRequest {
                payload: vec![0xEF; n],
            })
            .collect();
        let expect: Vec<Vec<u8>> = reqs.iter().map(|r| r.payload.clone()).collect();
        let resp = client
            .exchange(tonic::Request::new(tokio_stream::iter(reqs)))
            .await
            .expect("tonic→grpcx-TLS exchange failed");
        let mut stream = resp.into_inner();
        let mut got = Vec::new();
        while let Some(msg) = stream.message().await.expect("exchange stream errored") {
            got.push(msg.payload);
        }
        assert_eq!(got, expect, "bidi echo mismatch over TLS");
        println!("tonic-TLS-client → grpcx-server: bidi OK (3 msgs)");
    });

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

fn main() {
    presence_wire_compat();
    proto2_wire_compat();
    grpcx_client_to_tonic_server();
    tonic_client_to_grpcx_server();
    grpcx_client_to_tonic_tls_server();
    tonic_client_to_grpcx_tls_server();
    println!("INTEROP OK (both directions, h2c + TLS)");
}
