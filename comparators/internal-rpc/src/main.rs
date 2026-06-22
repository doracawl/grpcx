//! T5.4 — bench harness for the internal-RPC v1 stack.
//!
//! Drives every gate listed in `_dev/RFC-internal-rpc-arch.md` § 8,
//! emits a JSON result file under `results/`. See `README.md` for
//! the CLI surface + reference-hardware contract.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use grpcx::{
    CallContext, Metadata, RpcError, Server, Service, ShmemConnection, Status, StatusCode,
    TcpMuxV2Connection, UdsConnection,
};

// ============================================================
// CLI
// ============================================================

#[derive(Debug, Clone)]
struct Args {
    gate: String,
    samples: usize,
    payload: usize,
    throughput_bytes: usize,
    output: Option<PathBuf>,
    /// Opt-in busy-poll window (µs) for the tcp-mux v2
    /// client. `0` keeps the default blocking-wait
    /// behaviour. Mirrors
    /// `TcpMuxV2Connection::set_busy_poll_threshold`.
    tcpmux_busy_poll_us: u64,
    /// Opt-in **server-side** user-space busy-poll mode
    /// for tcp-mux v2 (`Server::listen_tcp_mux_v2_busy_
    /// poll_until`). Bypasses SQPOLL + linux_iouring
    /// fallbacks; server thread spins on `WouldBlock`.
    tcpmux_server_busy_poll: bool,
    /// CPU to pin the server thread to (Linux only; via
    /// `sched_setaffinity(2)`). `None` = no pin.
    /// Stabilises `shmem-rtt-busy` bimodal scheduling on
    /// cloud VMs by forcing producer / consumer onto
    /// known SMT siblings (e.g. `--pin-server 8
    /// --pin-client 0` on i7-10700K where cores 0,8 share
    /// L1 + L2).
    pin_server: Option<usize>,
    /// CPU to pin the client thread to. See `pin_server`.
    pin_client: Option<usize>,
}

impl Args {
    fn from_env() -> Self {
        let mut gate = String::from("all");
        let mut samples = 10_000usize;
        let mut payload = 64usize;
        let mut throughput_bytes = 1_000_000_000usize;
        let mut output: Option<PathBuf> = None;
        let mut tcpmux_busy_poll_us: u64 = 0;
        let mut tcpmux_server_busy_poll = false;
        let mut pin_server: Option<usize> = None;
        let mut pin_client: Option<usize> = None;

        let argv: Vec<String> = env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            match a.as_str() {
                "--gate" => {
                    gate = argv.get(i + 1).cloned().expect("--gate needs a value");
                    i += 2;
                }
                "--samples" => {
                    samples = argv
                        .get(i + 1)
                        .expect("--samples needs a value")
                        .parse()
                        .expect("--samples must be a positive integer");
                    i += 2;
                }
                "--payload" => {
                    payload = argv
                        .get(i + 1)
                        .expect("--payload needs a value")
                        .parse()
                        .expect("--payload must be a positive integer");
                    i += 2;
                }
                "--throughput-bytes" => {
                    throughput_bytes = argv
                        .get(i + 1)
                        .expect("--throughput-bytes needs a value")
                        .parse()
                        .expect("--throughput-bytes must be a positive integer");
                    i += 2;
                }
                "--output" => {
                    output = Some(PathBuf::from(
                        argv.get(i + 1).cloned().expect("--output needs a value"),
                    ));
                    i += 2;
                }
                "--tcpmux-busy-poll-us" => {
                    tcpmux_busy_poll_us = argv
                        .get(i + 1)
                        .expect("--tcpmux-busy-poll-us needs a value")
                        .parse()
                        .expect("--tcpmux-busy-poll-us must be a non-negative integer");
                    i += 2;
                }
                "--tcpmux-server-busy-poll" => {
                    tcpmux_server_busy_poll = true;
                    i += 1;
                }
                "--pin-server" => {
                    pin_server = Some(
                        argv.get(i + 1)
                            .expect("--pin-server needs a CPU index")
                            .parse()
                            .expect("--pin-server must be a CPU index"),
                    );
                    i += 2;
                }
                "--pin-client" => {
                    pin_client = Some(
                        argv.get(i + 1)
                            .expect("--pin-client needs a CPU index")
                            .parse()
                            .expect("--pin-client must be a CPU index"),
                    );
                    i += 2;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => panic!("unknown arg: {other}; pass --help"),
            }
        }
        Self {
            gate,
            samples,
            payload,
            throughput_bytes,
            output,
            tcpmux_busy_poll_us,
            tcpmux_server_busy_poll,
            pin_server,
            pin_client,
        }
    }
}

/// Pin the current thread to the given CPU on Linux via
/// `sched_setaffinity(2)`. No-op on other platforms.
fn pin_current_thread(cpu: usize) {
    #[cfg(target_os = "linux")]
    {
        unsafe {
            // cpu_set_t is opaque to us; on glibc it's
            // 1024 bits = 128 bytes. Use a generous 256-B
            // buffer to be conservative across libcs.
            let mut set = [0u8; 256];
            let byte_idx = cpu / 8;
            let bit_idx = cpu % 8;
            if byte_idx < set.len() {
                set[byte_idx] |= 1 << bit_idx;
            }
            unsafe extern "C" {
                fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u8) -> i32;
            }
            let r = sched_setaffinity(0, set.len(), set.as_ptr());
            if r != 0 {
                eprintln!("warning: sched_setaffinity({cpu}) failed");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cpu;
    }
}

fn print_help() {
    println!(
        "internal-rpc-bench — T5.4 perf harness\n\n\
         --gate <name>          all (default) | shmem-rtt-busy | shmem-rtt-park |\n\
                                shmem-throughput | uds-rtt | tcpmux-rtt | codegen-1000m\n\
         --samples N            RTT samples after warm-up (default 10000)\n\
         --payload N            echo body size for RTT runs (default 64)\n\
         --throughput-bytes N   bytes per shmem throughput run (default 1e9)\n\
         --output FILE          result-file path override\n\
         --tcpmux-busy-poll-us N\n\
                                opt-in busy-poll window (µs) for the tcp-mux v2\n\
                                client (default 0 = blocking wait)\n"
    );
}

// ============================================================
// JSON emitter — pure std, no serde.
// ============================================================

struct JsonObj(Vec<(String, String)>);

impl JsonObj {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn str(&mut self, k: &str, v: &str) -> &mut Self {
        self.0.push((k.to_string(), json_string(v)));
        self
    }
    fn num<N: std::fmt::Display>(&mut self, k: &str, v: N) -> &mut Self {
        self.0.push((k.to_string(), format!("{v}")));
        self
    }
    fn obj(&mut self, k: &str, child: JsonObj) -> &mut Self {
        self.0.push((k.to_string(), child.render()));
        self
    }
    fn render(self) -> String {
        let mut s = String::from("{");
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&json_string(k));
            s.push_str(": ");
            s.push_str(v);
        }
        s.push('}');
        s
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ============================================================
// Stats — median + p99 over a slice of nanos.
// ============================================================

struct Stats {
    samples: usize,
    min_ns: u64,
    median_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    mean_ns: u64,
}

impl Stats {
    fn from_ns(mut v: Vec<u64>) -> Self {
        v.sort_unstable();
        let n = v.len();
        let median = v[n / 2];
        let p99 = v[(n * 99) / 100];
        let mean = (v.iter().sum::<u64>()) / n as u64;
        Self {
            samples: n,
            min_ns: *v.first().unwrap(),
            median_ns: median,
            p99_ns: p99,
            max_ns: *v.last().unwrap(),
            mean_ns: mean,
        }
    }

    fn to_json(&self) -> JsonObj {
        let mut o = JsonObj::new();
        o.num("samples", self.samples)
            .num("min_ns", self.min_ns)
            .num("median_ns", self.median_ns)
            .num("p99_ns", self.p99_ns)
            .num("max_ns", self.max_ns)
            .num("mean_ns", self.mean_ns);
        o
    }
}

// ============================================================
// Echo service — single name across every transport.
// ============================================================

struct EchoSvc;

impl Service for EchoSvc {
    fn name(&self) -> &str {
        "bench.Echo"
    }
    fn dispatch(&self, _: &str, _: &Metadata, _: &[u8]) -> Result<(Vec<u8>, Metadata), RpcError> {
        Err(RpcError::Status(Status {
            code: StatusCode::Internal,
            message: "use dispatch_with_ctx".into(),
        }))
    }
    fn dispatch_with_ctx(
        &self,
        method: &str,
        ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        match method {
            // `bulk` — server-streaming throughput probe.
            // Request body's first 4 bytes hold a u32 LE
            // frame count; reply ships that many copies of
            // the rest of the request body as separate DATA
            // frames. Drives a single-RPC push without
            // RTT-bounding the bench.
            //
            // Hot-path elision: instead of materialising N
            // body clones (Vec<Vec<u8>>), return a single
            // body Vec and stamp `x-broadcast-body = N` on
            // the trailing metadata. The shmem `send_response`
            // recognises the marker and pushes the same
            // body slice N times, sharing one allocation.
            // Eliminates ~190 ns/frame of dispatch-time
            // alloc + memcpy on the throughput hot path.
            "bulk" => {
                let req = requests.first().cloned().unwrap_or_default();
                if req.len() < 4 {
                    return Err(RpcError::Status(Status {
                        code: StatusCode::InvalidArgument,
                        message: "bulk: missing frame count prefix".into(),
                    }));
                }
                let n = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
                let body = req[4..].to_vec();
                ctx.add_trailing("x-broadcast-body", &n.to_le_bytes());
                Ok(vec![body])
            }
            // Default: unary echo.
            _ => Ok(vec![requests.first().cloned().unwrap_or_default()]),
        }
    }
}

// ============================================================
// Gate implementations.
// ============================================================

fn rng_payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i & 0xff) as u8).collect()
}

fn tmp_path(tag: &str) -> PathBuf {
    let mut p = env::temp_dir();
    p.push(format!(
        "grpcx-bench-{}-{}-{}.sock",
        tag,
        std::process::id(),
        tag.len()
    ));
    let _ = fs::remove_file(&p);
    p
}

fn ephemeral_addr() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    format!("127.0.0.1:{port}")
}

/// Start an in-process Server with a single EchoSvc on a background
/// thread; returns `(stop_flag, join_handle)`.
fn spawn_server<F>(
    serve: F,
    pin_cpu: Option<usize>,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>)
where
    F: FnOnce(Server, Arc<AtomicBool>) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let mut server = Server::new();
    server.register(Box::new(EchoSvc));
    let h = std::thread::spawn(move || {
        if let Some(cpu) = pin_cpu {
            pin_current_thread(cpu);
        }
        serve(server, stop_clone);
    });
    (stop, h)
}

// -- shmem RTT (busy-poll vs park) ----------------------------

fn gate_shmem_rtt(args: &Args, busy_poll: bool) -> Stats {
    let sock = tmp_path(if busy_poll {
        "shmem-busy"
    } else {
        "shmem-park"
    });
    let sock_clone = sock.clone();
    let (stop, handle) = spawn_server(
        move |s, stop| {
            let _ = s.listen_shmem_until(&sock_clone, &stop);
        },
        args.pin_server,
    );
    if let Some(cpu) = args.pin_client {
        pin_current_thread(cpu);
    }

    let mut conn = connect_with_retry(|| ShmemConnection::connect(&sock, "bench"));
    // RFC § 8 splits "busy-poll on" vs "park fallback only".
    // Busy-poll on = a generous spin threshold so the reader
    // never reaches the wake-fd park; park fallback = Duration::ZERO
    // (the default — no spin, every wait hits the futex).
    conn.set_busy_poll_threshold(if busy_poll {
        Duration::from_millis(100)
    } else {
        Duration::ZERO
    });
    let body = rng_payload(args.payload);

    // Warm-up.
    for _ in 0..2_000 {
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("warm");
    }
    let mut nanos = Vec::with_capacity(args.samples);
    for _ in 0..args.samples {
        let t0 = Instant::now();
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("rtt");
        nanos.push(t0.elapsed().as_nanos() as u64);
    }

    stop.store(true, Ordering::Relaxed);
    drop(conn);
    let _ = handle.join();
    let _ = fs::remove_file(&sock);
    Stats::from_ns(nanos)
}

// -- shmem throughput ----------------------------------------

fn gate_shmem_throughput(args: &Args) -> JsonObj {
    let sock = tmp_path("shmem-thru");
    let sock_clone = sock.clone();
    let (stop, handle) = spawn_server(
        move |s, stop| {
            let _ = s.listen_shmem_busy_poll_until(&sock_clone, &stop);
        },
        args.pin_server,
    );
    if let Some(cpu) = args.pin_client {
        pin_current_thread(cpu);
    }
    let mut conn = connect_with_retry(|| ShmemConnection::connect(&sock, "bench"));
    conn.set_busy_poll_unbounded(true);

    // 3 KiB body per frame — fits in one 4 KiB shmem slot
    // (header + metadata overhead consume the rest).
    // CONTINUATION-fragmented sends across multiple slots
    // are a v1.x surface; the throughput gate measures the
    // single-slot fast path.
    let body_size: usize = 3 * 1024;
    let target = args.throughput_bytes;
    let n_frames: u32 = (target as u64).div_ceil(body_size as u64) as u32;

    // bulk request: `[u32 LE n_frames | body_size bytes of body template]`.
    let mut req = Vec::with_capacity(4 + body_size);
    req.extend_from_slice(&n_frames.to_le_bytes());
    req.extend(rng_payload(body_size));

    let t0 = Instant::now();
    let moved = std::cell::Cell::new(0u64);
    let _trailing = conn
        .call_server_streaming_view(
            "bench.Echo",
            "bulk",
            &Metadata::default(),
            &req,
            |slot_payload| {
                moved.set(moved.get() + slot_payload.len() as u64);
            },
        )
        .expect("thru server stream view");
    let elapsed = t0.elapsed();
    let moved: u64 = moved.get();

    stop.store(true, Ordering::Relaxed);
    drop(conn);
    let _ = handle.join();
    let _ = fs::remove_file(&sock);

    let secs = elapsed.as_secs_f64();
    let gbps = (moved as f64 / 1e9) / secs;
    let mut o = JsonObj::new();
    o.num("bytes_moved", moved)
        .num("elapsed_ns", elapsed.as_nanos() as u64)
        .num("body_size", body_size as u64)
        .num("throughput_gbps", format!("{gbps:.3}"));
    o
}

// -- UDS RTT -------------------------------------------------

fn gate_uds_rtt(args: &Args) -> Stats {
    let sock = tmp_path("uds");
    let sock_clone = sock.clone();
    let (stop, handle) = spawn_server(
        move |s, stop| {
            let _ = s.listen_uds_until(&sock_clone, &stop);
        },
        args.pin_server,
    );
    if let Some(cpu) = args.pin_client {
        pin_current_thread(cpu);
    }
    let mut conn = connect_with_retry(|| UdsConnection::connect(&sock, "bench"));
    let body = rng_payload(args.payload);

    for _ in 0..2_000 {
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("warm");
    }
    let mut nanos = Vec::with_capacity(args.samples);
    for _ in 0..args.samples {
        let t0 = Instant::now();
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("rtt");
        nanos.push(t0.elapsed().as_nanos() as u64);
    }

    stop.store(true, Ordering::Relaxed);
    drop(conn);
    let _ = handle.join();
    let _ = fs::remove_file(&sock);
    Stats::from_ns(nanos)
}

// -- tcp-mux v2 RTT ------------------------------------------

fn gate_tcpmux_rtt(args: &Args) -> Stats {
    let addr = ephemeral_addr();
    let addr_clone = addr.clone();
    let server_busy_poll = args.tcpmux_server_busy_poll;
    let (stop, handle) = spawn_server(
        move |s, stop| {
            if server_busy_poll {
                let _ = s.listen_tcp_mux_v2_busy_poll_until(&addr_clone, &stop);
            } else {
                let _ = s.listen_tcp_mux_v2_until(&addr_clone, &stop);
            }
        },
        args.pin_server,
    );
    if let Some(cpu) = args.pin_client {
        pin_current_thread(cpu);
    }
    let mut conn = connect_with_retry(|| TcpMuxV2Connection::connect(&addr, "bench"));
    if args.tcpmux_busy_poll_us > 0 {
        conn.set_busy_poll_threshold(std::time::Duration::from_micros(args.tcpmux_busy_poll_us));
    }
    let body = rng_payload(args.payload);

    for _ in 0..2_000 {
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("warm");
    }
    let mut nanos = Vec::with_capacity(args.samples);
    for _ in 0..args.samples {
        let t0 = Instant::now();
        let _ = conn
            .call_unary("bench.Echo", "echo", &Metadata::default(), &body)
            .expect("rtt");
        nanos.push(t0.elapsed().as_nanos() as u64);
    }

    stop.store(true, Ordering::Relaxed);
    drop(conn);
    let _ = handle.join();
    Stats::from_ns(nanos)
}

// -- grpcx-pb codegen, 1000-method `.proto` ------------------

fn gate_codegen_1000m() -> JsonObj {
    // Build a synthetic 1000-rpc `.proto` source and time
    // import + codegen end-to-end.
    let mut src = String::from(
        "syntax = \"proto3\";\npackage bench;\n\nmessage Req { string s = 1; }\nmessage Rep { string s = 1; }\n\nservice Big {\n",
    );
    for i in 0..1000 {
        src.push_str(&format!("  rpc M{i} (Req) returns (Rep);\n"));
    }
    src.push_str("}\n");

    let t0 = Instant::now();
    let schema = grpcx_pb::import(&src).expect("import 1000-method proto");
    let import_ns = t0.elapsed().as_nanos() as u64;

    let t1 = Instant::now();
    let _generated = grpcx_pb::codegen(&schema);
    let codegen_ns = t1.elapsed().as_nanos() as u64;

    let total_ns = import_ns + codegen_ns;
    let mut o = JsonObj::new();
    o.num("methods", 1000u32)
        .num("import_ns", import_ns)
        .num("codegen_ns", codegen_ns)
        .num("total_ns", total_ns);
    o
}

// ============================================================
// Connect with bind-race retry.
// ============================================================

fn connect_with_retry<C, F>(mut connect: F) -> C
where
    F: FnMut() -> Result<C, RpcError>,
{
    for _ in 0..400 {
        if let Ok(c) = connect() {
            return c;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("bench: server never bound after 2s")
}

// ============================================================
// Hardware fingerprint + result-file naming.
// ============================================================

fn fingerprint() -> JsonObj {
    let host = env::var("HOSTNAME").unwrap_or_else(|_| {
        // hostname(2) — read via uname-equivalent on Unix.
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    });
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let bench_timestamp = env::var("BENCH_TIMESTAMP")
        .or_else(|_| env::var("SOURCE_DATE_EPOCH"))
        .unwrap_or_else(|_| "0".into());

    let mut o = JsonObj::new();
    o.str("hostname", &host)
        .str("os", &os)
        .str("arch", &arch)
        .num("cpu_count", cpu_count)
        .str("cargo_profile", profile)
        .str("git_sha", &git_sha)
        .str("bench_timestamp", &bench_timestamp);
    o
}

fn results_path(args: &Args) -> PathBuf {
    if let Some(p) = &args.output {
        return p.clone();
    }
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let arch = std::env::consts::ARCH;
    let ts = env::var("BENCH_TIMESTAMP")
        .or_else(|_| env::var("SOURCE_DATE_EPOCH"))
        .unwrap_or_else(|_| "0".into());
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut p = PathBuf::from(manifest_dir);
    p.push("results");
    p.push(format!("{host}-{arch}-{ts}.json"));
    p
}

// ============================================================
// Main
// ============================================================

fn main() {
    let args = Args::from_env();
    let gates: Vec<&str> = if args.gate == "all" {
        vec![
            "shmem-rtt-busy",
            "shmem-rtt-park",
            "shmem-throughput",
            "uds-rtt",
            "tcpmux-rtt",
            "codegen-1000m",
        ]
    } else {
        vec![args.gate.as_str()]
    };

    let mut root = JsonObj::new();
    root.obj("fingerprint", fingerprint());
    let mut config = JsonObj::new();
    config
        .num("samples", args.samples)
        .num("payload", args.payload)
        .num("throughput_bytes", args.throughput_bytes);
    root.obj("config", config);

    for g in &gates {
        eprintln!("== running gate: {g}");
        let result = match *g {
            "shmem-rtt-busy" => gate_shmem_rtt(&args, true).to_json(),
            "shmem-rtt-park" => gate_shmem_rtt(&args, false).to_json(),
            "shmem-throughput" => gate_shmem_throughput(&args),
            "uds-rtt" => gate_uds_rtt(&args).to_json(),
            "tcpmux-rtt" => gate_tcpmux_rtt(&args).to_json(),
            "codegen-1000m" => gate_codegen_1000m(),
            other => panic!("unknown gate: {other}"),
        };
        root.obj(g, result);
    }

    let json = root.render();
    let path = results_path(&args);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir results");
    }
    let mut f = fs::File::create(&path).expect("create result file");
    f.write_all(json.as_bytes()).expect("write json");
    f.write_all(b"\n").expect("trailing newline");
    println!("{}", path.display());
    println!("{json}");
}

// `Path` is only used for type inference inside `tmp_path`'s
// callees; keep the use to avoid an unused-import warning when
// the cfg surface tightens further.
const _: Option<&Path> = None;
