//! Performance regression gates.
//!
//! Each `#[test]` asserts the measured P95 ≤ the budget declared in
//! `BUDGETS.md`. CI passes `--skip _under_budget` to bypass these
//! tests (cloud runners are too jittery for stable µs-scale timing).
//! Run the gate locally on a quiet machine.

#![cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]

use core::time::Duration;
use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2};
use grpcx_io::{Events, Interest, Poll, SourceFd, Token};
use std::time::Instant;

const WARMUP_ITERS: usize = 1_000;
const SAMPLE_ITERS: usize = 10_000;

fn p95(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    let idx = ((samples.len() as f64) * 0.95) as usize;
    samples[idx.min(samples.len() - 1)]
}

#[test]
fn poll_idle_zero_timeout_under_budget() {
    let mut poll = Poll::new().expect("Poll::new");
    let mut events = Events::with_capacity(64);
    let timeout = Some(Duration::ZERO);

    for _ in 0..WARMUP_ITERS {
        poll.poll(&mut events, timeout).expect("poll");
    }

    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLE_ITERS);
    for _ in 0..SAMPLE_ITERS {
        let t = Instant::now();
        poll.poll(&mut events, timeout).expect("poll");
        samples.push(t.elapsed());
    }

    let measured = p95(&mut samples);
    // Per-OS budget — Darwin's `kevent()` is ~150× slower than
    // Linux's `epoll_pwait()` for an empty wait; budgets reflect
    // that reality rather than impose a uniform ceiling. See
    // BUDGETS.md.
    #[cfg(target_os = "linux")]
    let budget = Duration::from_nanos(1_000);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let budget = Duration::from_micros(20);
    // FreeBSD on KVM observed ~159 ns (much closer to Linux native
    // 94 ns than to Darwin's ~12 µs — Darwin's `kevent` path is
    // syscall-overhead-heavy, FreeBSD/Linux are not). Budget at
    // ~6× observed for VM jitter headroom; tune lower once
    // sustained CI numbers are in.
    #[cfg(target_os = "freebsd")]
    let budget = Duration::from_nanos(1_000);
    assert!(
        measured < budget,
        "poll_idle p95 = {:?}, budget = {:?}",
        measured,
        budget,
    );
}

/// Round-trip a batch of 256 pipe reads through `io_uring` and
/// assert the steady-state p95 stays under the budget. The budget
/// is the v0.4 gate: io_uring at N=256 must beat (or match) epoll's
/// equivalent path — see `BUDGETS.md`.
#[test]
#[cfg(target_os = "linux")]
fn uring_pipe_drain_n256_under_budget() {
    use grpcx_io::{Ring, Sqe};

    const N: usize = 256;
    const WARMUP: usize = 200;
    const SAMPLES: usize = 1_000;

    let pipes: Vec<(i32, i32)> = (0..N).map(|_| pipe2(O_CLOEXEC).expect("pipe2")).collect();
    let mut ring = Ring::new(N.next_power_of_two() as u32).expect("ring");
    let mut bufs = vec![0u8; N];
    let buf_ptr = bufs.as_mut_ptr();

    let one_iter = |ring: &mut Ring| {
        for &(_, w) in &pipes {
            let _ = grpcx_io::__bench_helpers::write(w, b"x");
        }
        for (i, &(r, _)) in pipes.iter().enumerate() {
            // SAFETY: see compare_uring.rs — buf_ptr.add(i) is a
            // single-byte writable region for the duration of the
            // submit_and_wait + reap below.
            let sqe = unsafe { Sqe::read(r, buf_ptr.add(i), 1, u64::MAX, i as u64) };
            ring.push_sqe(&sqe).expect("push");
        }
        ring.submit_and_wait(N as u32, N as u32).expect("submit");
        ring.reap_cqes(|_| ());
    };

    for _ in 0..WARMUP {
        one_iter(&mut ring);
    }

    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t = Instant::now();
        one_iter(&mut ring);
        samples.push(t.elapsed());
    }

    let measured = p95(&mut samples);
    // Budget: 130 µs. Observed criterion median on lx64
    // (i7-10700K @ 3.80 GHz, kernel 6.12) is ~92 µs; a 1.4× ceiling
    // catches a meaningful regression without false-positives on
    // day-to-day noise. See BUDGETS.md for the full table.
    let budget = Duration::from_nanos(130_000);
    assert!(
        measured < budget,
        "uring_pipe_drain_n256 p95 = {:?}, budget = {:?}",
        measured,
        budget,
    );

    for &(r, w) in &pipes {
        let _ = close(r);
        let _ = close(w);
    }
}

#[test]
fn register_then_deregister_under_budget() {
    let poll = Poll::new().expect("Poll::new");
    let registry = poll.registry().clone();
    let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
    let mut src = SourceFd::new(rfd);

    for _ in 0..WARMUP_ITERS {
        registry
            .register(&mut src, Token(0), Interest::READABLE)
            .expect("register");
        registry.deregister(&mut src).expect("deregister");
    }

    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLE_ITERS);
    for _ in 0..SAMPLE_ITERS {
        let t = Instant::now();
        registry
            .register(&mut src, Token(0), Interest::READABLE)
            .expect("register");
        registry.deregister(&mut src).expect("deregister");
        samples.push(t.elapsed());
    }

    let measured = p95(&mut samples);
    // Linux: ~506 ns observed. Darwin: ~458 ns observed (kqueue's
    // `EV_RECEIPT` fast path lands inside one syscall). Both
    // budgets target ~3× the observed median.
    #[cfg(target_os = "linux")]
    let budget = Duration::from_nanos(2_000);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let budget = Duration::from_nanos(1_500);
    // FreeBSD on KVM observed ~444 ns — within 2% of mio's 451 ns
    // on the same VM (effectively tied). Budget ~3× observed,
    // matches Darwin's 1.5 µs for cross-OS parity.
    #[cfg(target_os = "freebsd")]
    let budget = Duration::from_nanos(1_500);
    assert!(
        measured < budget,
        "register/deregister p95 = {:?}, budget = {:?}",
        measured,
        budget,
    );

    let _ = close(rfd);
    let _ = close(wfd);
}

/// Per-event decoder cost (translate-only, no syscall): assert
/// each decoder call's amortized cost stays under a budget that
/// catches a regression but not VM noise. Observed via criterion:
/// ~250-300 ps on apple silicon mac and linux x86_64. We can't
/// measure individual calls (Instant::now overhead is ~40 ns
/// itself), so we time a batch of 1000 calls and assert the
/// amortized per-call cost.
#[test]
fn event_translate_amortized_under_budget() {
    use grpcx_io::__fuzz_helpers::decode_one_event;

    // Same 64-byte buffer the criterion bench uses; size covers
    // every backend's struct.
    let raw: [u8; 64] = {
        let mut b = [0u8; 64];
        b[0] = 0x01;
        b[8] = 0xff;
        b[16] = 0x01;
        b
    };

    const BATCH: usize = 1000;
    // Warmup so the cache + branch predictor are primed.
    for _ in 0..WARMUP_ITERS {
        let _ = core::hint::black_box(decode_one_event(&raw));
    }

    // Time `SAMPLE_ITERS` batches of `BATCH` calls each. Per-batch
    // wall time / BATCH = per-call cost. Use the median batch.
    let mut batch_times: Vec<Duration> = Vec::with_capacity(SAMPLE_ITERS / BATCH);
    for _ in 0..(SAMPLE_ITERS / BATCH) {
        let t = Instant::now();
        for _ in 0..BATCH {
            let _ = core::hint::black_box(decode_one_event(&raw));
        }
        batch_times.push(t.elapsed());
    }
    let measured_batch = p95(&mut batch_times);
    let measured_per_call = measured_batch / (BATCH as u32);

    // 50 ns per call — generous, catches regressions where the
    // translate path picks up a syscall or heap allocation while
    // letting jitter through.
    let budget = Duration::from_nanos(50);
    assert!(
        measured_per_call < budget,
        "event_translate per-call p95 = {:?} (batch {:?} ÷ {}), budget = {:?}",
        measured_per_call,
        measured_batch,
        BATCH,
        budget,
    );
}
