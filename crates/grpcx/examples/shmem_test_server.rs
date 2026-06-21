//! Standalone shmem test server for the TS cross-language
//! e2e suite. Listens on the AF_UNIX rendezvous path
//! passed as `argv[1]`, registers a small `TestService`,
//! and serves until SIGINT / SIGTERM.

#![cfg(feature = "transport-shmem")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grpcx::{CallContext, Metadata, RpcError, Server, Service, Status, StatusCode};

struct TestService;

impl Service for TestService {
    fn name(&self) -> &str {
        "ts_e2e.Echo"
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
        method: &str,
        _ctx: &mut CallContext,
        requests: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RpcError> {
        match method {
            "echo_unary" => Ok(vec![requests.first().cloned().unwrap_or_default()]),
            "fanout_3" => {
                let base = requests.first().cloned().unwrap_or_default();
                Ok((0..3)
                    .map(|i| {
                        let mut out = base.clone();
                        out.push(b'#');
                        out.push(b'0' + i);
                        out
                    })
                    .collect())
            }
            "concat" => {
                let mut out = Vec::new();
                for r in requests {
                    if !out.is_empty() {
                        out.push(b'+');
                    }
                    out.extend_from_slice(r);
                }
                Ok(vec![out])
            }
            "reply" => Ok(requests
                .iter()
                .map(|r| {
                    let mut out = b"re:".to_vec();
                    out.extend_from_slice(r);
                    out
                })
                .collect()),
            other => Err(RpcError::Status(Status {
                code: StatusCode::Unimplemented,
                message: format!("ts_shmem_e2e: unknown method {other}"),
            })),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let sock_path = PathBuf::from(args.next().expect("usage: shmem_test_server <path>"));
    if sock_path.exists()
        && let Ok(meta) = sock_path.symlink_metadata()
    {
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_socket() {
            let _ = std::fs::remove_file(&sock_path);
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    ctrlc_install(stop.clone());

    let mut server = Server::new();
    server.register(Box::new(TestService));
    let busy_poll = std::env::var("SHMEM_BUSY_POLL")
        .map(|v| v == "1")
        .unwrap_or(false);
    eprintln!(
        "shmem_test_server: listening on {} (busy_poll={})",
        sock_path.display(),
        busy_poll,
    );
    let _ = if busy_poll {
        server.listen_shmem_busy_poll_until(&sock_path, &stop)
    } else {
        server.listen_shmem_until(&sock_path, &stop)
    };
    let _ = std::fs::remove_file(&sock_path);
}

fn ctrlc_install(stop: Arc<AtomicBool>) {
    unsafe extern "C" fn handler(_: i32) {
        IN_FLIGHT_STOP.store(true, Ordering::Relaxed);
    }
    static IN_FLIGHT_STOP: AtomicBool = AtomicBool::new(false);
    let watcher_stop = stop.clone();
    std::thread::spawn(move || {
        while !IN_FLIGHT_STOP.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        watcher_stop.store(true, Ordering::Relaxed);
    });
    let h_ptr = handler as *const () as usize;
    unsafe {
        signal(SIGINT, h_ptr);
        signal(SIGTERM, h_ptr);
    }
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}
