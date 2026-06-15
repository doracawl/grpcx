//! Focused profiling harness: the real two-thread 64 KiB
//! unary path over UDP loopback — the exact shape the
//! bench's `grpcx-h3-udp` rows measure, minus tonic and
//! the small-payload lanes. Feed to `samply record` to
//! attribute the UDP-over-mem delta (client and server
//! threads sample in the same process).

mod budget;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Syscall-shape accounting: wraps a transport and counts
/// empty vs productive recv_batch calls (an empty call is
/// one wasted nonblocking syscall in the spin loop) plus
/// sends and parks, with cumulative wall time per bucket.
struct CountingTransport<T> {
    inner: T,
    recv_empty: u64,
    recv_empty_ns: u64,
    recv_full: u64,
    recv_full_ns: u64,
    recv_datagrams: u64,
    send_calls: u64,
    send_ns: u64,
    send_datagrams: u64,
    parks: u64,
    park_ns: u64,
}

impl<T> CountingTransport<T> {
    fn new(inner: T) -> Self {
        CountingTransport {
            inner,
            recv_empty: 0,
            recv_empty_ns: 0,
            recv_full: 0,
            recv_full_ns: 0,
            recv_datagrams: 0,
            send_calls: 0,
            send_ns: 0,
            send_datagrams: 0,
            parks: 0,
            park_ns: 0,
        }
    }
    fn reset(&mut self) {
        self.recv_empty = 0;
        self.recv_empty_ns = 0;
        self.recv_full = 0;
        self.recv_full_ns = 0;
        self.recv_datagrams = 0;
        self.send_calls = 0;
        self.send_ns = 0;
        self.send_datagrams = 0;
        self.parks = 0;
        self.park_ns = 0;
    }
    fn report(&self, label: &str, calls: u64) {
        let per = |v: u64| v as f64 / calls as f64;
        println!("--- {label} (per call over {calls}):");
        println!(
            "  recv empty : {:6.1} calls  {:7.2} us",
            per(self.recv_empty),
            per(self.recv_empty_ns) / 1000.0
        );
        println!(
            "  recv full  : {:6.1} calls  {:7.2} us  ({:.1} datagrams)",
            per(self.recv_full),
            per(self.recv_full_ns) / 1000.0,
            per(self.recv_datagrams)
        );
        println!(
            "  send       : {:6.1} calls  {:7.2} us  ({:.1} datagrams)",
            per(self.send_calls),
            per(self.send_ns) / 1000.0,
            per(self.send_datagrams)
        );
        println!(
            "  park       : {:6.1} calls  {:7.2} us",
            per(self.parks),
            per(self.park_ns) / 1000.0
        );
    }
}

impl<T: grpcx::DatagramTransport> grpcx::DatagramTransport for CountingTransport<T> {
    fn send(&mut self, datagram: &[u8]) -> Result<(), grpcx::RpcError> {
        let t = Instant::now();
        let r = self.inner.send(datagram);
        self.send_ns += t.elapsed().as_nanos() as u64;
        self.send_calls += 1;
        self.send_datagrams += 1;
        r
    }
    fn recv(&mut self) -> Result<Option<Vec<u8>>, grpcx::RpcError> {
        self.inner.recv()
    }
    fn send_batch(&mut self, datagrams: &[Vec<u8>]) -> Result<(), grpcx::RpcError> {
        let t = Instant::now();
        let r = self.inner.send_batch(datagrams);
        self.send_ns += t.elapsed().as_nanos() as u64;
        self.send_calls += 1;
        self.send_datagrams += datagrams.len() as u64;
        r
    }
    fn recv_batch(&mut self, out: &mut Vec<Vec<u8>>) -> Result<usize, grpcx::RpcError> {
        let t = Instant::now();
        let r = self.inner.recv_batch(out);
        let ns = t.elapsed().as_nanos() as u64;
        match &r {
            Ok(0) => {
                self.recv_empty += 1;
                self.recv_empty_ns += ns;
            }
            Ok(n) => {
                self.recv_full += 1;
                self.recv_full_ns += ns;
                self.recv_datagrams += *n as u64;
            }
            Err(_) => {}
        }
        r
    }
    fn wait_readable(&mut self, timeout: std::time::Duration) -> Result<bool, grpcx::RpcError> {
        let t = Instant::now();
        let r = self.inner.wait_readable(timeout);
        self.park_ns += t.elapsed().as_nanos() as u64;
        self.parks += 1;
        r
    }
}

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
}

fn main() {
    use grpcx::DatagramTransport as _;

    const SERVER_NAME: &str = "loopback.example";
    let params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_der, key_der) = (cert.der().to_vec(), kp.serialize_der());

    let mut server_cfg = grpcx_crypto::TlsConfig::server(&[&cert_der], &key_der).unwrap();
    server_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);
    let mut client_cfg = grpcx_crypto::TlsConfig::client();
    client_cfg.add_root_cert(&cert_der).unwrap();
    client_cfg.set_alpn(&[grpcx::ALPN_H3.as_bytes()]);

    let server_t_raw = grpcx::UdpTransport::bind("127.0.0.1:0", "127.0.0.1:1").unwrap();
    let server_addr = server_t_raw.local_addr().unwrap();
    let mut server_t = CountingTransport::new(server_t_raw);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = grpcx::Server::new();
        server.register(Box::new(GrpcxEcho));
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        conn.set_datagram_budget(budget::loopback_datagram_budget());
        let mut inbox: Vec<Vec<u8>> = Vec::new();
        let mut outs: Vec<Vec<u8>> = Vec::new();
        while !server_stop.load(Ordering::Relaxed) {
            let mut progressed = false;
            while server_t.recv_batch(&mut inbox).unwrap() > 0 {
                for d in inbox.drain(..) {
                    outs.extend(conn.pump(Some(&d)).unwrap());
                }
                progressed = true;
            }
            conn.dispatch(&server).unwrap();
            outs.extend(conn.pump(None).unwrap());
            if !outs.is_empty() {
                server_t.send_batch(&outs).unwrap();
                outs.clear();
                progressed = true;
            }
            if !progressed {
                server_t
                    .wait_readable(std::time::Duration::from_micros(500))
                    .unwrap();
            }
        }
        server_t
    });

    let mut client_t = CountingTransport::new(
        grpcx::UdpTransport::bind("127.0.0.1:0", &server_addr.to_string()).unwrap(),
    );
    let mut channel =
        grpcx::Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    channel
        .set_datagram_budget(budget::loopback_datagram_budget())
        .unwrap();
    let method = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Ping".to_string(),
        kind: grpcx::RpcKind::Unary,
    };

    // 64 KiB PingRequest, hand-encoded (field 1, LEN).
    let size = 65536usize;
    let mut req = vec![0x0A, 0x80, 0x80, 0x04]; // varint 65536
    req.extend(std::iter::repeat_n(0xAB, size));

    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    // Warmup (handshake + steady state).
    for _ in 0..200 {
        grpcx::call_unary(
            &mut channel,
            &mut client_t,
            &method,
            &grpcx::Metadata::default(),
            &req,
        )
        .unwrap();
    }

    client_t.reset();
    let t0 = Instant::now();
    for _ in 0..iters {
        let (resp, _) = grpcx::call_unary(
            &mut channel,
            &mut client_t,
            &method,
            &grpcx::Metadata::default(),
            &req,
        )
        .unwrap();
        assert_eq!(resp.len(), req.len());
    }
    let dt = t0.elapsed();
    println!(
        "{iters} x 64KiB unary over UDP in {:.1} ms = {:.1} us/call",
        dt.as_secs_f64() * 1e3,
        dt.as_secs_f64() * 1e6 / iters as f64
    );

    client_t.report("client", iters as u64);
    stop.store(true, Ordering::Relaxed);
    let server_t = server_thread.join().unwrap();
    // Server counters span warmup + timed (it can't see
    // the boundary) — normalize over all calls.
    server_t.report("server (incl. warmup)", 200 + iters as u64);
}
