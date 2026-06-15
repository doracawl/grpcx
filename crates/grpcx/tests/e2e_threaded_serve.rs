//! L2-Runtime serve acceptance (charter 2026-06-12): the
//! shared TCP accept loop dispatches connections on a
//! work-stealing runtime — one slow RPC must no longer
//! head-of-line block every other connection.
//!
//! Concurrency is observed INSIDE the service (an atomic
//! enter/exit counter recording its high-water mark), not
//! through wall-clock bounds — noise-immune. The sleep per
//! call stays well under the client's ~32 ms stall budget;
//! on the old single-thread loop the same load serializes
//! (high-water mark 1) and the tail clients stall out, so
//! this suite fails loudly there either way.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grpcx::{Metadata, Method, RpcError, RpcKind, Server, Service};

const SLEEP_MS: u64 = 5;
const CLIENTS: usize = 8;

struct SlowEcho {
    live: Arc<AtomicUsize>,
    high_water: Arc<AtomicUsize>,
}

impl Service for SlowEcho {
    fn name(&self) -> &str {
        "test.v1.SlowEcho"
    }
    fn dispatch(
        &self,
        _method: &str,
        _metadata: &Metadata,
        request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        let now = self.live.fetch_add(1, Ordering::AcqRel) + 1;
        self.high_water.fetch_max(now, Ordering::AcqRel);
        std::thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
        self.live.fetch_sub(1, Ordering::AcqRel);
        Ok((request.to_vec(), Metadata::default()))
    }
}

fn method() -> Method {
    Method {
        service: "test.v1.SlowEcho".to_string(),
        method: "Echo".to_string(),
        kind: RpcKind::Unary,
    }
}

fn free_port_addr() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    addr
}

/// Fire `CLIENTS` parallel unary calls via `connect`,
/// asserting byte-exact echoes; returns when all complete.
fn fire_clients(addr: &str, connect: fn(&str) -> Result<grpcx::ConnectedChannel, RpcError>) {
    let mut joins = Vec::new();
    for i in 0..CLIENTS {
        let addr = addr.to_string();
        joins.push(std::thread::spawn(move || {
            let mut conn = {
                let mut tries = 0;
                loop {
                    match connect(&addr) {
                        Ok(c) => break c,
                        Err(_) if tries < 200 => {
                            tries += 1;
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(e) => panic!("connect: {e:?}"),
                    }
                }
            };
            let payload = format!("client-{i}");
            let (resp, _) = conn
                .call_unary(&method(), &Metadata::default(), payload.as_bytes())
                .expect("call");
            assert_eq!(resp, payload.as_bytes(), "echo must be byte-exact");
        }));
    }
    for j in joins {
        j.join().expect("client thread");
    }
}

fn run_parallelism_case(
    listen: impl FnOnce(Server, String, Arc<AtomicBool>) + Send + 'static,
    connect: fn(&str) -> Result<grpcx::ConnectedChannel, RpcError>,
) {
    let addr = free_port_addr();
    let live = Arc::new(AtomicUsize::new(0));
    let high_water = Arc::new(AtomicUsize::new(0));

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let svc = SlowEcho {
        live: live.clone(),
        high_water: high_water.clone(),
    };
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(svc));
        listen(server, server_addr, server_stop);
    });

    fire_clients(&addr, connect);

    let observed = high_water.load(Ordering::Acquire);
    assert!(
        observed >= 2,
        "dispatch must overlap across connections (observed high-water {observed})"
    );

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn slow_rpcs_are_served_in_parallel_h2c() {
    run_parallelism_case(
        |server, addr, stop| server.listen_h2c_until(&addr, &stop).unwrap(),
        |addr| grpcx::ConnectedChannel::connect_h2c(addr, addr),
    );
}

#[test]
fn slow_rpcs_are_served_in_parallel_tcp_mux() {
    run_parallelism_case(
        |server, addr, stop| server.listen_tcp_mux_until(&addr, &stop).unwrap(),
        |addr| grpcx::ConnectedChannel::connect_tcp_mux(addr, addr),
    );
}

/// Registering after a serve loop holds the registry must
/// panic with the documented message (and re-open once the
/// loop stops).
#[test]
fn register_while_serving_panics() {
    fn slow_echo() -> SlowEcho {
        SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }
    }
    let mut server = Server::new();
    server.register(Box::new(slow_echo()));
    let shared = server.clone(); // simulates a live serve loop's clone
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        server.register(Box::new(slow_echo()));
    }))
    .is_err();
    assert!(panicked, "register with a live clone must panic");
    drop(shared);
    server.register(Box::new(slow_echo())); // unique again — re-opens
    assert_eq!(server.service_count(), 2);
}

/// Close-out measurement (run explicitly: `--ignored`):
/// an idle threaded server must park, not yield-spin —
/// process CPU over 3 s idle should be ~ms, not ~3 s per
/// spinning thread. Asserted at a generous 0.5 s so
/// machine noise can't flake it.
#[test]
#[ignore = "timing probe — run explicitly for the close-out number"]
fn idle_threaded_server_parks_instead_of_spinning() {
    #[repr(C)]
    struct Timeval(i64, i32);
    #[repr(C)]
    struct Rusage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        _rest: [u8; 14 * 8],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut core::ffi::c_void) -> i32;
    }
    fn cpu_time_s() -> f64 {
        let mut ru: Rusage = unsafe { core::mem::zeroed() };
        unsafe { getrusage(0, &mut ru as *mut _ as *mut core::ffi::c_void) };
        ru.ru_utime.0 as f64
            + ru.ru_utime.1 as f64 / 1e6
            + ru.ru_stime.0 as f64
            + ru.ru_stime.1 as f64 / 1e6
    }

    let addr = free_port_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });
    // One connection so a conn task is parked too, then idle.
    let _conn = {
        let mut tries = 0;
        loop {
            match grpcx::ConnectedChannel::connect_h2c(&addr, &addr) {
                Ok(c) => break c,
                Err(_) if tries < 200 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("connect: {e:?}"),
            }
        }
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let c0 = cpu_time_s();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let used = cpu_time_s() - c0;
    println!("idle 3 s: {used:.3} s CPU");
    assert!(used < 0.5, "idle server burned {used:.3} s CPU in 3 s");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

/// THE liveness headline (charter 2026-06-12): the exact
/// scenario that surfaced the stall/deadline conflation —
/// a handler far slower than the old ~32 ms IDLE_STALL,
/// hit by 8 concurrent clients. Liveness now belongs to
/// the transport (the link is demonstrably alive), so
/// every call must complete.
#[test]
fn slow_handlers_no_longer_stall_clients() {
    struct VerySlowEcho;
    impl Service for VerySlowEcho {
        fn name(&self) -> &str {
            "test.v1.SlowEcho"
        }
        fn dispatch(
            &self,
            _m: &str,
            _md: &Metadata,
            request: &[u8],
        ) -> Result<(Vec<u8>, Metadata), RpcError> {
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok((request.to_vec(), Metadata::default()))
        }
    }

    let addr = free_port_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(VerySlowEcho));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    fire_clients(&addr, |addr| {
        grpcx::ConnectedChannel::connect_h2c(addr, addr)
    });

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

/// Connection reuse across an idle gap longer than the
/// (shrunk) liveness windows: gap forgiveness on the
/// client + liveness-off server defaults must keep the
/// connection usable.
#[test]
fn idle_connection_survives_between_calls() {
    let addr = free_port_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    let mut conn = {
        let mut tries = 0;
        loop {
            match grpcx::ConnectedChannel::connect_h2c(&addr, &addr) {
                Ok(c) => break c,
                Err(_) if tries < 200 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("connect: {e:?}"),
            }
        }
    };
    // Windows sized for loaded CI boxes: 60 ms idle used
    // to flake under full-suite parallel load on the
    // 16-core Linux box (a scheduling stall mid-call
    // tripped it). 400 ms keeps the semantics — the gap
    // below is still 3x the idle timeout.
    conn.channel_mut()
        .set_link_liveness(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(400),
        )
        .unwrap();
    let (r1, _) = conn
        .call_unary(&method(), &Metadata::default(), b"first")
        .expect("first call");
    assert_eq!(r1, b"first");
    // Idle far past the idle timeout — nobody pumps the
    // blocking client between calls; the next pump's clock
    // jump must be forgiven, not declared a dead link.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let (r2, _) = conn
        .call_unary(&method(), &Metadata::default(), b"second")
        .expect("call after idle gap must succeed");
    assert_eq!(r2, b"second");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

/// h3/UDP form (charter 2026-06-12, multi-core listen_h3):
/// router thread + task-per-conn — same high-water assert
/// as the TCP cases, over real UDP sockets with TLS.
#[test]
fn slow_rpcs_are_served_in_parallel_h3() {
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
    let client_cfg = Arc::new(client_cfg);

    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let live = Arc::new(AtomicUsize::new(0));
    let high_water = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let svc = SlowEcho {
        live: live.clone(),
        high_water: high_water.clone(),
    };
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(svc));
        server
            .listen_h3_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    let mut joins = Vec::new();
    for i in 0..CLIENTS {
        let addr = addr.clone();
        let client_cfg = Arc::clone(&client_cfg);
        joins.push(std::thread::spawn(move || {
            // UDP has no connect()-time reachability signal:
            // racing the server's bind surfaces at the FIRST
            // call instead (Linux delivers the ICMP
            // port-unreachable as ECONNREFUSED on the
            // connected socket — parity with the TCP forms'
            // connect-refused, where the sibling tests retry
            // connect()). Retry the connect+first-call pair
            // with a FRESH conn each time: the old socket
            // keeps the queued ICMP error.
            let payload = format!("client-{i}");
            let mut tries = 0;
            let resp = loop {
                let attempt = grpcx::ConnectedChannel::connect_h3(&addr, &client_cfg, SERVER_NAME)
                    .and_then(|mut conn| {
                        conn.call_unary(&method(), &Metadata::default(), payload.as_bytes())
                    });
                match attempt {
                    Ok((resp, _)) => break resp,
                    Err(_) if tries < 200 => {
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(e) => panic!("connect+call: {e:?}"),
                }
            };
            assert_eq!(resp, payload.as_bytes(), "echo must be byte-exact");
        }));
    }
    for j in joins {
        j.join().expect("client thread");
    }

    let observed = high_water.load(Ordering::Acquire);
    assert!(
        observed >= 2,
        "h3 dispatch must overlap across connections (observed high-water {observed})"
    );

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

// ============================================================
// Passive idle GC (0.25): max_connection_idle reaps quiet
// conns; active conns and the no-GC default are untouched.
// ============================================================

#[test]
fn idle_conn_is_reaped_when_gc_enabled_h2c() {
    let addr = free_port_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }));
        server.set_max_connection_idle(Some(std::time::Duration::from_millis(100)));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    let mut conn = {
        let mut tries = 0;
        loop {
            match grpcx::ConnectedChannel::connect_h2c(&addr, &addr) {
                Ok(c) => break c,
                Err(_) if tries < 200 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("connect: {e:?}"),
            }
        }
    };
    let (r, _) = conn
        .call_unary(&method(), &Metadata::default(), b"one")
        .expect("first call");
    assert_eq!(r, b"one");

    // Idle far past the budget — the server reaps the conn.
    std::thread::sleep(std::time::Duration::from_millis(600));
    let meta = Metadata {
        headers: vec![
            grpcx::GrpcTimeout::from_duration(std::time::Duration::from_millis(500)).to_header(),
        ],
    };
    assert!(
        conn.call_unary(&method(), &meta, b"two").is_err(),
        "reaped connection must not serve another call"
    );

    // A FRESH connection works — the server itself lives.
    let mut conn2 = grpcx::ConnectedChannel::connect_h2c(&addr, &addr).expect("reconnect");
    let (r, _) = conn2
        .call_unary(&method(), &Metadata::default(), b"three")
        .expect("fresh conn after reap");
    assert_eq!(r, b"three");

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn active_conn_survives_gc_budget_h2c() {
    let addr = free_port_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }));
        server.set_max_connection_idle(Some(std::time::Duration::from_millis(200)));
        server.listen_h2c_until(&server_addr, &server_stop).unwrap();
    });

    let mut conn = {
        let mut tries = 0;
        loop {
            match grpcx::ConnectedChannel::connect_h2c(&addr, &addr) {
                Ok(c) => break c,
                Err(_) if tries < 200 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("connect: {e:?}"),
            }
        }
    };
    // Keep traffic flowing well inside the 200 ms budget;
    // the conn must never be reaped mid-conversation.
    for i in 0..6 {
        let payload = format!("tick-{i}");
        let (r, _) = conn
            .call_unary(&method(), &Metadata::default(), payload.as_bytes())
            .expect("active conn must survive");
        assert_eq!(r, payload.as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}

#[test]
fn idle_conn_is_reaped_when_gc_enabled_h3() {
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

    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_addr = addr.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(SlowEcho {
            live: Arc::new(AtomicUsize::new(0)),
            high_water: Arc::new(AtomicUsize::new(0)),
        }));
        server.set_max_connection_idle(Some(std::time::Duration::from_millis(100)));
        server
            .listen_h3_until(&server_addr, &server_cfg, &server_stop)
            .unwrap();
    });

    // connect+first-call with retry (the UDP bind race —
    // see the parallelism test above).
    let mut tries = 0;
    let mut conn = loop {
        let attempt = grpcx::ConnectedChannel::connect_h3(&addr, &client_cfg, SERVER_NAME)
            .and_then(|mut c| {
                c.call_unary(&method(), &Metadata::default(), b"one")
                    .map(|_| c)
            });
        match attempt {
            Ok(c) => break c,
            Err(_) if tries < 200 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("connect+call: {e:?}"),
        }
    };

    std::thread::sleep(std::time::Duration::from_millis(600));
    // The server-side session is gone; the client's call
    // can only fail (deadline-bounded so the test stays
    // fast — the reaped peer never answers).
    let meta = Metadata {
        headers: vec![
            grpcx::GrpcTimeout::from_duration(std::time::Duration::from_millis(400)).to_header(),
        ],
    };
    assert!(
        conn.call_unary(&method(), &meta, b"two").is_err(),
        "reaped h3 session must not serve another call"
    );

    stop.store(true, Ordering::Relaxed);
    server_thread.join().unwrap();
}
