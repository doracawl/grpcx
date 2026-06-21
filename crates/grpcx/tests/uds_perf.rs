//! `transport-uds` performance smoke — `#[ignore]` by
//! default; run with:
//!
//! ```text
//! cargo test --release --features transport-uds \
//!     --test uds_perf -- --ignored --nocapture
//! ```
//!
//! The RFC § 4 ceiling target is **< 5 µs median** unary
//! RTT on reference hardware (Linux 6.x, 8-core x86_64,
//! `taskset`-pinned). This file is a sanity smoke, not
//! the gating bench — the criterion-based gating bench
//! lives under T5.4 (`comparators/internal-rpc/`).

#![cfg(feature = "transport-uds")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use grpcx::{CallContext, Metadata, RpcError, Server, Service, Status, StatusCode, UdsConnection};

struct EchoSvc;

impl Service for EchoSvc {
    fn name(&self) -> &str {
        "uds_perf.Echo"
    }
    fn dispatch(
        &self,
        _method: &str,
        _meta: &Metadata,
        _request: &[u8],
    ) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "use dispatch_with_ctx".into(),
        }))
    }
    fn dispatch_with_ctx(
        &self,
        _method: &str,
        _ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        Ok(vec![requests.first().cloned().unwrap_or_default()])
    }
}

fn unique_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("grpcx-uds-perf-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
#[ignore]
fn unary_rtt_smoke() {
    let path = unique_path();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let path_for_server = path.clone();
    let server_thread = std::thread::spawn(move || {
        let mut server = Server::new();
        server.register(Box::new(EchoSvc));
        let _ = server.listen_uds_until(&path_for_server, &stop_clone);
    });

    // Wait for bind.
    let mut conn = loop {
        if let Ok(c) = UdsConnection::connect(&path, "localhost") {
            break c;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Warm-up — first call also opens kernel buffers.
    for _ in 0..1_000 {
        let _ = conn
            .call_unary("uds_perf.Echo", "ping", &Metadata::default(), b"warm")
            .unwrap();
    }

    // Measure.
    const ITERS: usize = 10_000;
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let _ = conn
            .call_unary("uds_perf.Echo", "ping", &Metadata::default(), b"x")
            .unwrap();
        samples.push(t0.elapsed());
    }
    samples.sort();
    let p50 = samples[ITERS / 2];
    let p90 = samples[ITERS * 9 / 10];
    let p99 = samples[ITERS * 99 / 100];
    let mean: Duration = samples.iter().sum::<Duration>() / ITERS as u32;

    println!("uds unary RTT over {ITERS} iters (--release recommended):");
    println!("  mean = {mean:?}");
    println!("  p50  = {p50:?}");
    println!("  p90  = {p90:?}");
    println!("  p99  = {p99:?}");
    println!("  (RFC § 4 ceiling: p50 < 5 µs on reference HW)");

    stop.store(true, Ordering::Relaxed);
    let _ = server_thread.join();
    let _ = std::fs::remove_file(&path);
}
