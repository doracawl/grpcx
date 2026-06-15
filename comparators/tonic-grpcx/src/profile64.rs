//! Focused profiling harness: the pure-stack (mem
//! loopback) 64 KiB unary path, high iteration count —
//! feed to `samply record` to find where the ~229 µs per
//! call goes. No tonic, no sockets, no second thread.

mod budget;

use std::time::Instant;

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

    let mut channel =
        grpcx::Channel::connect_h3(&client_cfg, SERVER_NAME, "loopback.example:443").unwrap();
    let mut server = grpcx::Server::new();
    server.register(Box::new(GrpcxEcho));
    let mut conn = server.accept_h3(&server_cfg).unwrap();

    let mut to_server: std::collections::VecDeque<Vec<u8>> = Default::default();
    let mut to_client: std::collections::VecDeque<Vec<u8>> = Default::default();

    // Handshake.
    for _ in 0..64 {
        let incoming = to_client.pop_front();
        for d in channel.pump(incoming.as_deref()).unwrap() {
            to_server.push_back(d);
        }
        let incoming = to_server.pop_front();
        for d in conn.pump(incoming.as_deref()).unwrap() {
            to_client.push_back(d);
        }
        conn.dispatch(&server).unwrap();
        if channel.is_ready() && conn.is_ready() {
            break;
        }
    }
    assert!(channel.is_ready());
    let b = budget::loopback_datagram_budget();
    channel.set_datagram_budget(b).unwrap();
    conn.set_datagram_budget(b);

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
        .unwrap_or(2000);

    let t0 = Instant::now();
    for _ in 0..iters {
        let mut call = channel
            .begin_unary(&method, &grpcx::Metadata::default(), &req)
            .unwrap();
        let mut done = false;
        for _ in 0..100_000 {
            let incoming = to_client.pop_front();
            for d in channel.pump(incoming.as_deref()).unwrap() {
                to_server.push_back(d);
            }
            if let Some((resp, _)) = channel.poll_unary(&mut call).unwrap() {
                assert_eq!(resp.len(), req.len());
                done = true;
                break;
            }
            let incoming = to_server.pop_front();
            for d in conn.pump(incoming.as_deref()).unwrap() {
                to_client.push_back(d);
            }
            conn.dispatch(&server).unwrap();
        }
        assert!(done, "call did not complete");
    }
    let dt = t0.elapsed();
    println!(
        "{iters} x 64KiB unary in {:.1} ms = {:.1} us/call",
        dt.as_secs_f64() * 1e3,
        dt.as_secs_f64() * 1e6 / iters as f64
    );
}
