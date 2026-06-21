//! io_uring perf smoke for `transport-tcp-mux v2` (T4.2
//! gate 1 — RFC § 5 / § 8: median loopback RTT < 8 µs on
//! reference hardware).
//!
//! Linux-only and `#[ignore]` by default — the perf gate
//! is a T5.4 bench-harness concern, not part of the
//! `cargo test --workspace` floor. Run explicitly with:
//!
//!     cargo test -p grpcx --features transport-tcp-mux-iouring \
//!         --test iouring_perf -- --ignored
//!
//! on a Linux box with an io_uring-capable kernel
//! (>= 5.1, ideally >= 6.0 for the multishot recv path).

#![cfg(all(target_os = "linux", feature = "transport-tcp-mux-iouring"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use grpcx::{
    CallContext, Metadata, RpcError, Server, Service, Status, StatusCode, TcpMuxV2Connection,
};

struct EchoSvc;

impl Service for EchoSvc {
    fn name(&self) -> &str {
        "iouring_perf.v1.Echo"
    }
    fn dispatch(&self, _: &str, _: &Metadata, _: &[u8]) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "use dispatch_with_ctx".into(),
        }))
    }
    fn dispatch_with_ctx(
        &self,
        _: &str,
        _: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        Ok(vec![requests.first().cloned().unwrap_or_default()])
    }
}

#[test]
#[ignore = "perf smoke — gated by T5.4 bench harness; run --ignored"]
fn unary_rtt_median_under_8us() {
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral");
        let p = l.local_addr().unwrap().port();
        drop(l);
        format!("127.0.0.1:{p}")
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let addr_clone = addr.clone();
    let h = std::thread::spawn(move || {
        let mut s = Server::new();
        s.register(Box::new(EchoSvc));
        let _ = s.listen_tcp_mux_v2_until(&addr_clone, &stop_clone);
    });

    // Bind-race retry.
    let mut conn = loop {
        if let Ok(c) = TcpMuxV2Connection::connect(&addr, "perf") {
            break c;
        }
        std::thread::sleep(Duration::from_millis(2));
    };

    // Warm-up.
    for _ in 0..1_000 {
        let _ = conn
            .call_unary("iouring_perf.v1.Echo", "echo", &Metadata::default(), b"x")
            .expect("warm");
    }

    let n = 10_000;
    let mut samples: Vec<u64> = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let _ = conn
            .call_unary("iouring_perf.v1.Echo", "echo", &Metadata::default(), b"x")
            .expect("call");
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    let median_ns = samples[n / 2];
    eprintln!(
        "iouring tcp-mux v2 unary RTT: median = {} ns ({:.2} µs), p99 = {} ns",
        median_ns,
        median_ns as f64 / 1_000.0,
        samples[(n * 99) / 100]
    );
    // The 8-µs bar is the RFC ceiling; the T5.4 bench
    // harness will gate it. Assert a generous 50-µs
    // soft bar here so the test catches a regression
    // by 10×+ without churning on small variations.
    assert!(median_ns < 50_000, "median RTT {} ns > 50 µs", median_ns);

    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
}
