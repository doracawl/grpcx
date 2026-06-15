//! Eager cancel acceptance (0.22, charter 2026-06-12): a
//! tripped deadline cancels the call ON THE WIRE — the
//! server reaps the request before dispatch ever sees it,
//! and the connection survives for the next call.
//!
//! Deterministic three-phase shape: (1) the server pump
//! thread runs with dispatch GATED OFF while the client's
//! deadline expires and the cancel frames cross; (2) the
//! gate opens — dispatch must find nothing (the counting
//! handler stays at zero); (3) a fresh, undeadlined call
//! on the SAME connection round-trips.

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

type Queue = Arc<Mutex<VecDeque<Vec<u8>>>>;

struct QueueTransport {
    out: Queue,
    inn: Queue,
}

impl grpcx::DatagramTransport for QueueTransport {
    fn send(&mut self, datagram: &[u8]) -> Result<(), RpcError> {
        self.out.lock().unwrap().push_back(datagram.to_vec());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(self.inn.lock().unwrap().pop_front())
    }
}

struct CountingEcho {
    dispatched: Arc<AtomicUsize>,
}

impl Service for CountingEcho {
    fn name(&self) -> &str {
        "test.v1.CountingEcho"
    }
    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        self.dispatched.fetch_add(1, Ordering::AcqRel);
        Ok((request.to_vec(), Metadata::default()))
    }
}

fn method() -> Method {
    Method {
        service: "test.v1.CountingEcho".to_string(),
        method: "Echo".to_string(),
        kind: RpcKind::Unary,
    }
}

#[test]
fn h3_deadline_cancel_reaps_request_before_dispatch() {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());
    let mut server_cfg = TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[ALPN_H3.as_bytes()]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[ALPN_H3.as_bytes()]);

    let to_server: Queue = Arc::new(Mutex::new(VecDeque::new()));
    let to_client: Queue = Arc::new(Mutex::new(VecDeque::new()));

    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatch_open = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    // Server pump thread: pumps the conn continuously,
    // but only calls dispatch once the gate opens.
    let server_thread = {
        let (to_server, to_client) = (to_server.clone(), to_client.clone());
        let (dispatched, dispatch_open, stop) =
            (dispatched.clone(), dispatch_open.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut server = Server::new();
            server.register(Box::new(CountingEcho { dispatched }));
            let mut conn = server.accept_h3(&server_cfg).unwrap();
            while !stop.load(Ordering::Relaxed) {
                let incoming = to_server.lock().unwrap().pop_front();
                for d in conn.pump(incoming.as_deref()).unwrap() {
                    to_client.lock().unwrap().push_back(d);
                }
                if dispatch_open.load(Ordering::Relaxed) {
                    conn.dispatch(&server).unwrap();
                    for d in conn.pump(None).unwrap() {
                        to_client.lock().unwrap().push_back(d);
                    }
                }
                std::thread::yield_now();
            }
        })
    };

    let channel = Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let transport = QueueTransport {
        out: to_server.clone(),
        inn: to_client.clone(),
    };
    let mut conn = ConnectedChannel::new(channel, Box::new(transport));

    // Phase 1: dispatch gated OFF — the call can only die
    // by deadline; the cancel frames cross to the server.
    let deadline_meta = Metadata {
        headers: vec![
            grpcx::GrpcTimeout::from_duration(std::time::Duration::from_millis(50)).to_header(),
        ],
    };
    let t0 = std::time::Instant::now();
    let verdict = conn.call_unary(&method(), &deadline_meta, b"doomed");
    match verdict {
        Err(RpcError::Status(s)) => {
            assert_eq!(s.code, grpcx::StatusCode::DeadlineExceeded, "{s:?}")
        }
        other => panic!("expected DEADLINE_EXCEEDED, got {other:?}"),
    }
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "verdict must come from the deadline, not a stall"
    );

    // Let the cancel frames drain into the server's pump.
    let drained = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < drained && !to_server.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Phase 2: open the gate — the reaped request must
    // never reach the handler.
    dispatch_open.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(
        dispatched.load(Ordering::Acquire),
        0,
        "cancelled-before-dispatch request must never run the handler"
    );

    // Phase 3: the SAME connection serves a fresh call.
    let (resp, _) = conn
        .call_unary(&method(), &Metadata::default(), b"alive")
        .expect("post-cancel reuse");
    assert_eq!(resp, b"alive");
    assert_eq!(dispatched.load(Ordering::Acquire), 1);

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
