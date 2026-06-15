//! ws-mux over TLS 1.3 (charter 2026-06-12): the fourth
//! transport's TLS form — `connect_ws_mux_tls` ↔
//! `listen_ws_mux_tls_until`, the WS upgrade + mux grammar
//! riding `TlsTransport` over real TCP loopback. Mirrors
//! the h2-TLS acceptance: shapes + deadline/reuse +
//! untrusted-cert negative (no cleartext fallback).

#![cfg(all(feature = "transport-ws-mux", feature = "transport-h2-tls"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grpcx::{GrpcTimeout, Metadata, Method, RpcError, RpcKind, Server, Service, StatusCode};
use grpcx_crypto::TlsConfig;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

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

fn mint() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn start_tls_server(
    cert_der: &[u8],
    key_der: &[u8],
) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let mut server_cfg = TlsConfig::server(&[cert_der], key_der).unwrap();
    server_cfg.set_alpn(&[b"grpcx-ws-mux"]);
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
            .listen_ws_mux_tls_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });
    (addr, stop, handle)
}

fn client_cfg(cert_der: &[u8]) -> TlsConfig {
    let mut cfg = TlsConfig::client();
    cfg.add_root_cert(cert_der).unwrap();
    cfg.set_alpn(&[b"grpcx-ws-mux"]);
    cfg
}

fn connect_with_retry(addr: &str, cfg: &TlsConfig) -> grpcx::ConnectedChannel {
    for _ in 0..200 {
        if let Ok(c) = grpcx::ConnectedChannel::connect_ws_mux_tls(addr, cfg, SERVER_NAME, addr) {
            return c;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("ws-mux TLS server at {addr} never came up");
}

#[test]
fn shapes_round_trip_over_tls() {
    let (cert, key) = mint();
    let (addr, stop, handle) = start_tls_server(&cert, &key);
    let mut conn = connect_with_retry(&addr, &client_cfg(&cert));

    let (resp, _) = conn
        .call_unary(
            &method("Echo", RpcKind::Unary),
            &Metadata::default(),
            b"ping",
        )
        .expect("unary");
    assert_eq!(resp, b"ping");

    // 64 KiB through the TLS record path (the
    // crypto-1.3.1 buffer-limit regression class).
    let big = vec![0xAB; 65536];
    let (resp, _) = conn
        .call_unary(&method("Echo", RpcKind::Unary), &Metadata::default(), &big)
        .expect("64 KiB unary");
    assert_eq!(resp, big);

    let (msgs, _) = conn
        .call_server_streaming(
            &method("Fanout", RpcKind::ServerStreaming),
            &Metadata::default(),
            b"x",
        )
        .expect("server streaming");
    assert_eq!(msgs, vec![b"x0".to_vec(), b"x1".to_vec(), b"x2".to_vec()]);

    let reqs: [&[u8]; 2] = [b"hi", b"yo"];
    let (msgs, _) = conn
        .call_bidi(
            &method("Chat", RpcKind::BidirectionalStreaming),
            &Metadata::default(),
            &reqs,
        )
        .expect("bidi");
    assert_eq!(msgs, vec![b"re:hi".to_vec(), b"re:yo".to_vec()]);

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn deadline_verdict_and_reuse_over_tls() {
    let (cert, key) = mint();
    let (addr, stop, handle) = start_tls_server(&cert, &key);
    let mut conn = connect_with_retry(&addr, &client_cfg(&cert));

    let meta = Metadata {
        headers: vec![GrpcTimeout::from_duration(std::time::Duration::from_millis(50)).to_header()],
    };
    match conn.call_unary(&method("Echo", RpcKind::Unary), &meta, b"slow") {
        Err(RpcError::Status(s)) => assert_eq!(s.code, StatusCode::DeadlineExceeded),
        other => panic!("expected DEADLINE_EXCEEDED, got {other:?}"),
    }
    let (resp, _) = conn
        .call_unary(
            &method("Echo", RpcKind::Unary),
            &Metadata::default(),
            b"after",
        )
        .expect("conn survives the verdict");
    assert_eq!(resp, b"after");

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn untrusted_cert_is_rejected_no_cleartext_fallback() {
    let (cert, key) = mint();
    let (addr, stop, handle) = start_tls_server(&cert, &key);

    // Trusted client first — proves the server is up, so
    // the untrusted failure below is a TRUST failure, not
    // a startup race.
    let mut good = connect_with_retry(&addr, &client_cfg(&cert));
    let (resp, _) = good
        .call_unary(&method("Echo", RpcKind::Unary), &Metadata::default(), b"ok")
        .expect("trusted client");
    assert_eq!(resp, b"ok");

    // A client that does NOT trust the server's cert: the
    // handshake must fail and the call must error — never
    // fall back to cleartext ws-mux.
    let mut cfg = TlsConfig::client();
    cfg.set_alpn(&[b"grpcx-ws-mux"]);
    let deadline_meta = Metadata {
        headers: vec![
            GrpcTimeout::from_duration(std::time::Duration::from_millis(500)).to_header(),
        ],
    };
    match grpcx::ConnectedChannel::connect_ws_mux_tls(&addr, &cfg, SERVER_NAME, &addr) {
        Err(_) => {} // failed at connect — fine
        Ok(mut conn) => {
            // Lazy handshake: the failure may surface at
            // the first call instead.
            match conn.call_unary(&method("Echo", RpcKind::Unary), &deadline_meta, b"x") {
                Err(_) => {}
                Ok(_) => panic!("untrusted cert must not yield a working connection"),
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}
