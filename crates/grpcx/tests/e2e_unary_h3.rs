//! L10 v0.3 acceptance — local loopback unary RPC over
//! `grpcx/h3` (work unit 3: "local loopback unary RPC
//! succeeds").
//!
//! Two harness shapes:
//! - single-thread alternating drive using the
//!   non-blocking primitives (`begin_unary` /
//!   `poll_unary` / `pump` / `dispatch`),
//! - two-thread blocking `call_unary` over a
//!   mutex-shared in-memory datagram pair.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use grpcx::{
    ALPN_H3, Channel, DatagramTransport, Metadata, Method, RpcError, RpcKind, Server, Service,
    Status, StatusCode,
};
use grpcx_crypto::TlsConfig;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    (cert.der().to_vec(), kp.serialize_der())
}

fn tls_pair() -> (TlsConfig, TlsConfig) {
    let (cert_der, key_der) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    (client_cfg, server_cfg)
}

// ============================================================
// Echo service.
// ============================================================

struct Echo;

impl Service for Echo {
    fn name(&self) -> &str {
        "test.v1.Echo"
    }

    fn dispatch(
        &self,
        method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        match method {
            "Echo" => Ok((
                request.to_vec(),
                Metadata {
                    headers: vec![("x-echoed".to_string(), b"1".to_vec())],
                },
            )),
            "Fail" => Err(RpcError::Status(Status {
                code: StatusCode::NotFound,
                message: "no such thing".to_string(),
            })),
            _ => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("unknown method {method}"),
            })),
        }
    }
}

fn echo_server() -> Server {
    let mut server = Server::new();
    server.register(Box::new(Echo));
    server
}

fn method(name: &str) -> Method {
    Method {
        service: "test.v1.Echo".to_string(),
        method: name.to_string(),
        kind: RpcKind::Unary,
    }
}

// ============================================================
// Single-thread alternating harness.
// ============================================================

struct Loopback {
    channel: Channel,
    conn: grpcx::ServerConn,
    server: Server,
    to_server: VecDeque<Vec<u8>>,
    to_client: VecDeque<Vec<u8>>,
}

impl Loopback {
    fn new() -> Self {
        let (client_cfg, server_cfg) = tls_pair();
        let channel =
            Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
        let server = echo_server();
        let conn = server.accept_h3(&server_cfg).unwrap();
        Loopback {
            channel,
            conn,
            server,
            to_server: VecDeque::new(),
            to_client: VecDeque::new(),
        }
    }

    /// One alternating cycle: client pump, server pump +
    /// dispatch.
    fn cycle(&mut self) {
        let incoming = self.to_client.pop_front();
        for d in self.channel.pump(incoming.as_deref()).unwrap() {
            self.to_server.push_back(d);
        }
        let incoming = self.to_server.pop_front();
        for d in self.conn.pump(incoming.as_deref()).unwrap() {
            self.to_client.push_back(d);
        }
        self.conn.dispatch(&self.server).unwrap();
    }

    fn establish(&mut self) {
        for _ in 0..64 {
            self.cycle();
            if self.channel.is_ready() && self.conn.is_ready() {
                return;
            }
        }
        panic!("handshake did not complete");
    }
}

#[test]
fn unary_echo_round_trip() {
    let mut lb = Loopback::new();
    lb.establish();

    let req_meta = Metadata {
        headers: vec![("x-request-id".to_string(), b"7".to_vec())],
    };
    let mut call = lb
        .channel
        .begin_unary(&method("Echo"), &req_meta, b"ping-payload")
        .unwrap();

    for _ in 0..128 {
        lb.cycle();
        if let Some((payload, trailing)) = lb.channel.poll_unary(&mut call).unwrap() {
            assert_eq!(payload, b"ping-payload");
            assert!(
                trailing
                    .headers
                    .iter()
                    .any(|(n, v)| n == "x-echoed" && v == b"1")
            );
            return;
        }
    }
    panic!("unary call did not complete");
}

#[test]
fn unary_error_status_propagates() {
    let mut lb = Loopback::new();
    lb.establish();

    let mut call = lb
        .channel
        .begin_unary(&method("Fail"), &Metadata::default(), b"")
        .unwrap();

    for _ in 0..128 {
        lb.cycle();
        match lb.channel.poll_unary(&mut call) {
            Ok(None) => continue,
            Ok(Some(_)) => panic!("Fail must not succeed"),
            Err(RpcError::Status(status)) => {
                assert_eq!(status.code, StatusCode::NotFound);
                assert_eq!(status.message, "no such thing");
                return;
            }
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }
    panic!("error did not propagate");
}

#[test]
fn unknown_service_gets_unimplemented() {
    let mut lb = Loopback::new();
    lb.establish();

    let bad = Method {
        service: "test.v1.Nope".to_string(),
        method: "X".to_string(),
        kind: RpcKind::Unary,
    };
    let mut call = lb
        .channel
        .begin_unary(&bad, &Metadata::default(), b"")
        .unwrap();

    for _ in 0..128 {
        lb.cycle();
        match lb.channel.poll_unary(&mut call) {
            Ok(None) => continue,
            Ok(Some(_)) => panic!("unknown service must not succeed"),
            Err(RpcError::Status(status)) => {
                assert_eq!(status.code, StatusCode::Unimplemented);
                return;
            }
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }
    panic!("error did not propagate");
}

// ============================================================
// Two-thread blocking harness (`call_unary` facade).
// ============================================================

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
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError> {
        self.tx.lock().unwrap().push_back(datagram.to_vec());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(self.rx.lock().unwrap().pop_front())
    }
}

#[test]
fn blocking_call_unary_over_real_udp_loopback() {
    // Real OS UDP sockets on 127.0.0.1 — the first
    // socket-backed RPC in the stack. Server thread locks
    // onto the first client datagram's source address.
    let (client_cfg, server_cfg) = tls_pair();

    let mut server_t = grpcx::UdpTransport::bind("127.0.0.1:0", "127.0.0.1:1").unwrap();
    let server_addr = server_t.local_addr().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let server = echo_server();
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        while !server_stop.load(Ordering::Relaxed) {
            let incoming = server_t.recv().unwrap();
            for d in conn.pump(incoming.as_deref()).unwrap() {
                server_t.send(&d).unwrap();
            }
            conn.dispatch(&server).unwrap();
            if incoming.is_none() {
                std::thread::yield_now();
            }
        }
    });

    let mut client_t = grpcx::UdpTransport::bind("127.0.0.1:0", &server_addr.to_string()).unwrap();
    let mut channel =
        Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let (payload, _) = grpcx::call_unary(
        &mut channel,
        &mut client_t,
        &method("Echo"),
        &Metadata::default(),
        b"udp-ping",
    )
    .unwrap();
    assert_eq!(payload, b"udp-ping");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn blocking_call_unary_over_threads() {
    let (client_cfg, server_cfg) = tls_pair();
    let (mut client_t, mut server_t) = MemTransport::pair();

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let server = echo_server();
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        while !server_stop.load(Ordering::Relaxed) {
            let incoming = server_t.recv().unwrap();
            for d in conn.pump(incoming.as_deref()).unwrap() {
                server_t.send(&d).unwrap();
            }
            conn.dispatch(&server).unwrap();
            if incoming.is_none() {
                std::thread::yield_now();
            }
        }
    });

    let mut channel =
        Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let (payload, trailing) = grpcx::call_unary(
        &mut channel,
        &mut client_t,
        &method("Echo"),
        &Metadata::default(),
        b"blocking-ping",
    )
    .unwrap();
    assert_eq!(payload, b"blocking-ping");
    assert!(trailing.headers.iter().any(|(n, _)| n == "x-echoed"));

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

// ============================================================
// Reliability phase 2 (L4 1.6.0): flight-tail loss must be
// recovered by the PTO timer, not reported as a stall.
// ============================================================

/// Eats the LAST short-header (1-RTT) datagram of the first
/// `drops` client bursts that carry any — the flight-tail
/// shape packet-threshold detection cannot see.
struct TailLossy {
    inner: MemTransport,
    drops: usize,
}

impl DatagramTransport for TailLossy {
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError> {
        self.inner.send(datagram)
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        self.inner.recv()
    }
    fn send_batch(&mut self, datagrams: &[Vec<u8>]) -> Result<(), RpcError> {
        let is_short = |d: &[u8]| !d.is_empty() && d[0] & 0x80 == 0;
        let last_short = datagrams.iter().rposition(|d| is_short(d));
        match last_short {
            Some(tail) if self.drops > 0 => {
                self.drops -= 1;
                for (i, d) in datagrams.iter().enumerate() {
                    if i != tail {
                        self.inner.send(d)?;
                    }
                }
                Ok(())
            }
            _ => self.inner.send_batch(datagrams),
        }
    }
}

#[test]
fn tail_loss_recovers_via_pto_not_stall() {
    let (client_cfg, server_cfg) = tls_pair();
    let (client_t, mut server_t) = MemTransport::pair();
    // Two tail drops: typically the post-handshake SETTINGS
    // flight's tail and the request flight's tail — each
    // recoverable only by the probe timer.
    let mut client_t = TailLossy {
        inner: client_t,
        drops: 2,
    };

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let server = echo_server();
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        while !server_stop.load(Ordering::Relaxed) {
            let incoming = server_t.recv().unwrap();
            for d in conn.pump(incoming.as_deref()).unwrap() {
                server_t.send(&d).unwrap();
            }
            conn.dispatch(&server).unwrap();
            if incoming.is_none() {
                std::thread::yield_now();
            }
        }
    });

    let mut channel =
        Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let (payload, _) = grpcx::call_unary(
        &mut channel,
        &mut client_t,
        &method("Echo"),
        &Metadata::default(),
        b"lossy-tail-ping",
    )
    .expect("tail loss must be PTO-recovered, not a stall");
    assert_eq!(payload, b"lossy-tail-ping");
    assert_eq!(client_t.drops, 0, "both tail drops must have fired");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
