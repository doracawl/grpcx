//! ECONNREFUSED policy (charter 2026-06-12): handshaking →
//! fast-fail with the named error (TCP-connect parity);
//! established → transient (a stray ICMP must not kill a
//! live connection — liveness/deadline own the verdict).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grpcx::{
    ALPN_H3, Channel, ConnectedChannel, Metadata, Method, RpcError, RpcKind, Server, Service,
};
use grpcx_crypto::TlsConfig;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

fn tls_pair() -> (TlsConfig, TlsConfig) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    (client_cfg, server_cfg)
}

struct Echo;
impl Service for Echo {
    fn name(&self) -> &str {
        "test.v1.Echo"
    }
    fn dispatch(
        &self,
        _m: &str,
        _md: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Ok((request.to_vec(), Metadata::default()))
    }
}

fn method() -> Method {
    Method {
        service: "test.v1.Echo".to_string(),
        method: "Echo".to_string(),
        kind: RpcKind::Unary,
    }
}

type Queue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// In-memory transport that can inject one ECONNREFUSED
/// per `inject` increment, mid-stream.
struct FlakyTransport {
    out: Queue,
    inn: Queue,
    inject: Arc<AtomicUsize>,
}

impl grpcx::DatagramTransport for FlakyTransport {
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError> {
        self.out.lock().unwrap().push_back(datagram.to_vec());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        if self.inject.load(Ordering::Acquire) > 0 {
            self.inject.fetch_sub(1, Ordering::AcqRel);
            // The exact string the UDP transport maps
            // ECONNREFUSED to.
            return Err(RpcError::Transport("udp: connection refused"));
        }
        Ok(self.inn.lock().unwrap().pop_front())
    }
}

#[test]
fn refused_after_established_is_transient() {
    let (client_cfg, server_cfg) = tls_pair();
    let to_server: Queue = Arc::new(Mutex::new(VecDeque::new()));
    let to_client: Queue = Arc::new(Mutex::new(VecDeque::new()));
    let inject = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let server_thread = {
        let (to_server, to_client, stop) = (to_server.clone(), to_client.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut server = Server::new();
            server.register(Box::new(Echo));
            let mut conn = server.accept_h3(&server_cfg).unwrap();
            while !stop.load(Ordering::Relaxed) {
                let incoming = to_server.lock().unwrap().pop_front();
                for d in conn.pump(incoming.as_deref()).unwrap() {
                    to_client.lock().unwrap().push_back(d);
                }
                conn.dispatch(&server).unwrap();
                std::thread::yield_now();
            }
        })
    };

    let channel = Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let transport = FlakyTransport {
        out: to_server,
        inn: to_client,
        inject: inject.clone(),
    };
    let mut conn = ConnectedChannel::new(channel, Box::new(transport));

    // Establish + first call clean.
    let (r, _) = conn
        .call_unary(&method(), &Metadata::default(), b"one")
        .expect("first call");
    assert_eq!(r, b"one");

    // Inject a refused mid-call on the ESTABLISHED
    // connection — must be swallowed, the call completes.
    inject.store(3, Ordering::Release);
    let (r, _) = conn
        .call_unary(&method(), &Metadata::default(), b"two")
        .expect("refused after established must be transient");
    assert_eq!(r, b"two");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn dead_port_fails_fast_with_a_named_error() {
    // Nobody listens here. The handshake-phase policy must
    // produce a FAST, named failure — "udp: connection
    // refused" where the kernel delivers the ICMP signal
    // (Linux reliably; BSD/mac stacks vary), or the
    // caller's deadline where it doesn't. A silent
    // multi-second stall is the failure mode this charter
    // forbids.
    let (client_cfg, _) = tls_pair();
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let mut conn = ConnectedChannel::connect_h3(&addr, &client_cfg, SERVER_NAME).unwrap();
    let meta = Metadata {
        headers: vec![
            grpcx::GrpcTimeout::from_duration(std::time::Duration::from_millis(300)).to_header(),
        ],
    };
    let t0 = std::time::Instant::now();
    match conn.call_unary(&method(), &meta, b"x") {
        Err(RpcError::Transport(m)) => {
            assert_eq!(m, "udp: connection refused", "named error, not opaque");
        }
        Err(RpcError::Status(s)) => {
            assert_eq!(
                s.code,
                grpcx::StatusCode::DeadlineExceeded,
                "the only acceptable alternative verdict: {s:?}"
            );
        }
        other => panic!("expected refused or deadline, got {other:?}"),
    }
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "must fail fast, not stall"
    );
}
