//! v10.0 GATING e2e comparison: grpcx (gRPC over HTTP/3
//! over real UDP loopback) vs tonic (gRPC over HTTP/2 over
//! real TCP loopback). Same echo workload; methodology
//! mirrors stones/comparators: warmup, then N timed
//! serial unary calls; report p50 / p99 / mean RPS.
//!
//! Output CSV: impl,payload,calls,p50_us,p99_us,rps

mod budget;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub mod echo {
    tonic::include_proto!("echo.v1");
}

const PAYLOAD_SIZES: &[usize] = &[16, 1024, 65536];
const WARMUP: usize = 500;
const CALLS: usize = 500;

fn percentiles(mut samples: Vec<u64>) -> (u64, u64, f64) {
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    let total_ns: u64 = samples.iter().sum();
    let rps = samples.len() as f64 / (total_ns as f64 / 1e9);
    (p50, p99, rps)
}

fn report(impl_name: &str, payload: usize, samples: Vec<u64>) {
    let n = samples.len();
    let (p50, p99, rps) = percentiles(samples);
    println!(
        "{impl_name},{payload},{n},{:.1},{:.1},{rps:.0}",
        p50 as f64 / 1000.0,
        p99 as f64 / 1000.0
    );
}

// ============================================================
// grpcx side — H3 over UDP loopback.
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
        // PingRequest and PingResponse share the wire
        // layout (bytes field 1) — echo verbatim.
        Ok((request.to_vec(), grpcx::Metadata::default()))
    }
}

fn run_grpcx() {
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

    let mut server_t = grpcx::UdpTransport::bind("127.0.0.1:0", "127.0.0.1:1").unwrap();
    let server_addr = server_t.local_addr().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = grpcx::Server::new();
        server.register(Box::new(GrpcxEcho));
        let mut conn = server.accept_h3(&server_cfg).unwrap();
        // Budget probed from the live maxdgram sysctl
        // (Darwin default 9216 ⇒ 8916) — same class of
        // segment size the tonic/TCP lane gets from the
        // kernel on lo0; a raised sysctl flows into the
        // run automatically.
        conn.set_datagram_budget(budget::loopback_datagram_budget());
        let mut inbox: Vec<Vec<u8>> = Vec::new();
        let mut outs: Vec<Vec<u8>> = Vec::new();
        while !server_stop.load(Ordering::Relaxed) {
            // Batch-drain the socket before dispatching;
            // replies accumulate and leave in batch
            // syscalls too (sendmsg_x on Darwin).
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
                // Quiet link: park in the kernel until the
                // next datagram (or 500 µs for the stop
                // flag) — the tokio-equivalent IO-drive
                // shape, not a yield-spin.
                server_t
                    .wait_readable(std::time::Duration::from_micros(500))
                    .unwrap();
            }
        }
    });

    let mut client_t = grpcx::UdpTransport::bind("127.0.0.1:0", &server_addr.to_string()).unwrap();
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

    for &size in PAYLOAD_SIZES {
        // prost-encode PingRequest { payload } by hand:
        // field 1, LEN. (Avoids depending on prost in the
        // grpcx lane.)
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

        for _ in 0..WARMUP {
            grpcx::call_unary(
                &mut channel,
                &mut client_t,
                &method,
                &grpcx::Metadata::default(),
                &req,
            )
            .unwrap();
        }
        let mut samples = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let t0 = Instant::now();
            let (resp, _) = grpcx::call_unary(
                &mut channel,
                &mut client_t,
                &method,
                &grpcx::Metadata::default(),
                &req,
            )
            .unwrap();
            samples.push(t0.elapsed().as_nanos() as u64);
            assert_eq!(resp.len(), req.len());
        }
        report("grpcx-h3-udp", size, samples);
    }

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

// ============================================================
// tonic side — H2 over TCP loopback.
// ============================================================

struct TonicEcho;

#[tonic::async_trait]
impl echo::echo_server::Echo for TonicEcho {
    async fn ping(
        &self,
        request: tonic::Request<echo::PingRequest>,
    ) -> Result<tonic::Response<echo::PingResponse>, tonic::Status> {
        Ok(tonic::Response::new(echo::PingResponse {
            payload: request.into_inner().payload,
        }))
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

fn run_tonic() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // `serve_with_incoming` bypasses tonic's own
        // TcpIncoming wrapper, so accepted sockets keep
        // Nagle ON — on Linux loopback that's a 40 ms
        // delayed-ACK stall per multi-write response.
        // Set TCP_NODELAY per accepted socket (this is
        // what tonic's `serve(addr)` path does itself).
        let incoming = tokio_stream::StreamExt::map(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            |s| {
                if let Ok(ref stream) = s {
                    stream.set_nodelay(true).ok();
                }
                s
            },
        );
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(echo::echo_server::EchoServer::new(TonicEcho))
                .serve_with_incoming(incoming),
        );

        // Give the server a beat to start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let chan = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .tcp_nodelay(true) // tonic default, stated explicitly for the harness record
            .connect()
            .await
            .unwrap();
        let mut client = echo::echo_client::EchoClient::new(chan);

        for &size in PAYLOAD_SIZES {
            let req = echo::PingRequest {
                payload: vec![0xAB; size],
            };
            for _ in 0..WARMUP {
                client.ping(tonic::Request::new(req.clone())).await.unwrap();
            }
            let mut samples = Vec::with_capacity(CALLS);
            for _ in 0..CALLS {
                let t0 = Instant::now();
                let resp = client.ping(tonic::Request::new(req.clone())).await.unwrap();
                samples.push(t0.elapsed().as_nanos() as u64);
                assert_eq!(resp.into_inner().payload.len(), size);
            }
            report("tonic-h2-tcp", size, samples);
        }
    });
}

fn run_tonic_tls() {
    // Same lane as run_tonic but over TLS — the FAIR
    // comparison for the encrypted grpcx-h3 lanes (the
    // h2c lane pays zero crypto; at 64 KiB AEAD is ~half
    // the grpcx per-call cost).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    const SERVER_NAME: &str = "loopback.example";
    let params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()]).unwrap();
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = params.self_signed(&kp).unwrap();
    let (cert_pem, key_pem) = (cert.pem(), kp.serialize_pem());

    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let incoming = tokio_stream::StreamExt::map(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            |s| {
                if let Ok(ref stream) = s {
                    stream.set_nodelay(true).ok();
                }
                s
            },
        );
        let identity = tonic::transport::Identity::from_pem(&cert_pem, &key_pem);
        tokio::spawn(
            tonic::transport::Server::builder()
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
                .unwrap()
                .add_service(echo::echo_server::EchoServer::new(TonicEcho))
                .serve_with_incoming(incoming),
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let tls = tonic::transport::ClientTlsConfig::new()
            .ca_certificate(tonic::transport::Certificate::from_pem(&cert_pem))
            .domain_name(SERVER_NAME);
        let chan = tonic::transport::Endpoint::from_shared(format!("https://{addr}"))
            .unwrap()
            .tcp_nodelay(true)
            .tls_config(tls)
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = echo::echo_client::EchoClient::new(chan);

        for &size in PAYLOAD_SIZES {
            let req = echo::PingRequest {
                payload: vec![0xAB; size],
            };
            for _ in 0..WARMUP {
                client.ping(tonic::Request::new(req.clone())).await.unwrap();
            }
            let mut samples = Vec::with_capacity(CALLS);
            for _ in 0..CALLS {
                let t0 = Instant::now();
                let resp = client.ping(tonic::Request::new(req.clone())).await.unwrap();
                samples.push(t0.elapsed().as_nanos() as u64);
                assert_eq!(resp.into_inner().payload.len(), size);
            }
            report("tonic-h2-tls", size, samples);
        }
    });
}

/// Single-thread in-memory loopback lane — no UDP, no
/// second thread. Isolates pure protocol-stack cost.
fn run_grpcx_mem() {
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

    macro_rules! cycle {
        () => {{
            let incoming = to_client.pop_front();
            for d in channel.pump(incoming.as_deref()).unwrap() {
                to_server.push_back(d);
            }
            let incoming = to_server.pop_front();
            for d in conn.pump(incoming.as_deref()).unwrap() {
                to_client.push_back(d);
            }
            conn.dispatch(&server).unwrap();
        }};
    }

    for _ in 0..64 {
        cycle!();
        if channel.is_ready() && conn.is_ready() {
            break;
        }
    }
    assert!(channel.is_ready());
    // Same segment budget as the UDP lane — the mem lane
    // reports pure protocol-stack cost, and an in-memory
    // link has no MTU at all.
    let b = budget::loopback_datagram_budget();
    channel.set_datagram_budget(b).unwrap();
    conn.set_datagram_budget(b);

    let method = grpcx::Method {
        service: "echo.v1.Echo".to_string(),
        method: "Ping".to_string(),
        kind: grpcx::RpcKind::Unary,
    };

    for &size in PAYLOAD_SIZES {
        let mut req = vec![0x0A];
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

        let run_one = |channel: &mut grpcx::Channel,
                       conn: &mut grpcx::ServerConn,
                       server: &grpcx::Server,
                       to_server: &mut std::collections::VecDeque<Vec<u8>>,
                       to_client: &mut std::collections::VecDeque<Vec<u8>>,
                       req: &[u8]| {
            let mut call = channel
                .begin_unary(&method, &grpcx::Metadata::default(), req)
                .unwrap();
            for _ in 0..100_000 {
                let incoming = to_client.pop_front();
                for d in channel.pump(incoming.as_deref()).unwrap() {
                    to_server.push_back(d);
                }
                if let Some((resp, _)) = channel.poll_unary(&mut call).unwrap() {
                    return resp;
                }
                let incoming = to_server.pop_front();
                for d in conn.pump(incoming.as_deref()).unwrap() {
                    to_client.push_back(d);
                }
                conn.dispatch(server).unwrap();
            }
            panic!("mem call did not complete");
        };

        for _ in 0..WARMUP {
            run_one(
                &mut channel,
                &mut conn,
                &server,
                &mut to_server,
                &mut to_client,
                &req,
            );
        }
        let mut samples = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let t0 = Instant::now();
            let resp = run_one(
                &mut channel,
                &mut conn,
                &server,
                &mut to_server,
                &mut to_client,
                &req,
            );
            samples.push(t0.elapsed().as_nanos() as u64);
            assert_eq!(resp.len(), req.len());
        }
        report("grpcx-h3-mem", size, samples);
    }
}

fn main() {
    println!("impl,payload,calls,p50_us,p99_us,rps");
    run_grpcx_mem();
    run_grpcx();
    run_tonic();
    run_tonic_tls();
}
