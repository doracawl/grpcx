//! Fuzz the `Poll` / `Registry` / `SourceFd` lifecycle by driving a
//! scripted sequence of operations from fuzz input.
//!
//! Each input byte encodes one operation. The fuzzer must never
//! observe a panic; the `IoResult` error path is exercised heavily
//! but never crashes the process.

#![no_main]

use libfuzzer_sys::fuzz_target;

use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2};
use grpcx_io::__fuzz_helpers::interest_from_bits;
use grpcx_io::{Events, Poll, SourceFd, Token};

use core::time::Duration;

fuzz_target!(|data: &[u8]| {
    // Reset state per invocation. cargo-fuzz reuses the binary,
    // so this counts only against the per-call wall budget.
    let Ok(mut poll) = Poll::new() else {
        return;
    };
    let Ok((rfd, wfd)) = pipe2(O_CLOEXEC) else {
        return;
    };
    let mut src = SourceFd::new(rfd);
    let mut events = Events::with_capacity(16);
    let registry = poll.registry().clone();

    // Cap iterations so a long fuzz input doesn't run wild. Lower
    // cap keeps per-call throughput high (libFuzzer wants >1000
    // exec/s for useful corpus growth).
    let max_ops = data.len().min(32);
    for &byte in data.iter().take(max_ops) {
        match byte & 0b0111 {
            0 => {
                let _ = registry.register(
                    &mut src,
                    Token((byte >> 3) as usize),
                    interest_from_bits(byte >> 3),
                );
            }
            1 => {
                let _ = registry.reregister(
                    &mut src,
                    Token((byte >> 3) as usize),
                    interest_from_bits(byte >> 3),
                );
            }
            2 => {
                let _ = registry.deregister(&mut src);
            }
            3 | 4 => {
                // Non-blocking poll only. A 1ms blocking poll
                // tanks fuzz throughput; readiness changes are
                // detected via the zero-timeout path.
                let _ = poll.poll(&mut events, Some(Duration::ZERO));
            }
            5 => {
                // Write a byte to the pipe so a subsequent poll
                // actually has work to report.
                let _ = grpcx_io::__bench_helpers::write(wfd, &[byte]);
            }
            6 | 7 => {
                // No-op slots — kept so the byte distribution maps
                // evenly across 8 cases. (An earlier revision tried
                // `read(rfd, ...)` on the read end here, but pipes
                // returned by `pipe2(O_CLOEXEC)` are blocking; a
                // read from an empty pipe hangs the fuzz target
                // forever, defeating libFuzzer's `-max_total_time`.)
            }
            _ => unreachable!(),
        }
    }

    let _ = close(rfd);
    let _ = close(wfd);
});
