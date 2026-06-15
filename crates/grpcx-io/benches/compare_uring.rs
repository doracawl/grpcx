//! Head-to-head: `grpcx-io`'s `io_uring` ring vs `mio`'s epoll on
//! identical batched pipe-read workloads.
//!
//! v0.4 trigger gate: `Ring` P95 ≤ `Poll` (epoll) P95 at N ≥ 256.
//! This bench is the measurement that proves (or disproves) it.
//!
//! Fairness invariants:
//!
//! - Same workload shape: N pipes, 1 byte per pipe per iteration.
//! - Same `pipe2` syscall via `__bench_helpers` on both sides.
//! - Both sides pay the refill-write cost inside the timed region;
//!   neither gains an unfair "pre-warmed buffer" advantage.

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn bench_pipe_drain(c: &mut Criterion) {
    use criterion::{BenchmarkId, Throughput};
    use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2, read as raw_read, write};

    let mut group = c.benchmark_group("compare_uring/pipe_drain");

    for &n in &[8usize, 64, 256] {
        group.throughput(Throughput::Elements(n as u64));

        let pipes: Vec<(i32, i32)> = (0..n).map(|_| pipe2(O_CLOEXEC).expect("pipe2")).collect();

        // -----------------------------------------------------------
        // grpcx-io / io_uring lane
        // -----------------------------------------------------------
        {
            use grpcx_io::{Ring, Sqe};

            let mut ring = Ring::new(n.next_power_of_two() as u32).expect("ring");
            let mut bufs = vec![0u8; n];
            let buf_ptr = bufs.as_mut_ptr();

            group.bench_with_input(BenchmarkId::new("grpcx_uring", n), &n, |b, &n| {
                b.iter(|| {
                    for &(_, w) in &pipes {
                        let _ = write(w, b"x");
                    }
                    for (i, &(r, _)) in pipes.iter().enumerate() {
                        // SAFETY: `buf_ptr.add(i)` points to byte i
                        // of `bufs`; valid for one byte of writes
                        // until the matching CQE is reaped below.
                        let sqe = unsafe { Sqe::read(r, buf_ptr.add(i), 1, u64::MAX, i as u64) };
                        ring.push_sqe(&sqe).expect("push");
                    }
                    ring.submit_and_wait(n as u32, n as u32).expect("submit");
                    let mut got = 0u32;
                    ring.reap_cqes(|_| got += 1);
                    debug_assert_eq!(got as usize, n);
                });
            });
        }

        // -----------------------------------------------------------
        // mio / epoll lane
        // -----------------------------------------------------------
        {
            use mio::unix::SourceFd as MioSourceFd;
            use mio::{Events, Interest, Poll, Token};

            let mut poll = Poll::new().expect("mio Poll");
            let mut events = Events::with_capacity(n.max(16));

            for (i, &(r, _)) in pipes.iter().enumerate() {
                let mut src = MioSourceFd(&r);
                poll.registry()
                    .register(&mut src, Token(i), Interest::READABLE)
                    .expect("register");
            }

            group.bench_with_input(BenchmarkId::new("mio_epoll", n), &n, |b, &n| {
                b.iter(|| {
                    for &(_, w) in &pipes {
                        let _ = write(w, b"x");
                    }
                    let mut got = 0usize;
                    while got < n {
                        poll.poll(&mut events, None).expect("poll");
                        for ev in events.iter() {
                            let fd = pipes[ev.token().0].0;
                            let mut byte = [0u8; 1];
                            let _ = raw_read(fd, &mut byte);
                            got += 1;
                        }
                    }
                });
            });

            for &(r, _) in &pipes {
                let mut src = MioSourceFd(&r);
                let _ = poll.registry().deregister(&mut src);
            }
        }

        for &(r, w) in &pipes {
            let _ = close(r);
            let _ = close(w);
        }
    }

    group.finish();
}

/// Allocation cost: `Ring::new(8)`. WARM tier — paid once per worker
/// thread that wants its own io_uring instance. Includes one
/// `io_uring_setup` syscall + 2-or-3 `mmap`s.
#[cfg(target_os = "linux")]
fn bench_ring_new(c: &mut Criterion) {
    use grpcx_io::Ring;

    let mut group = c.benchmark_group("ring_new");
    group.bench_function("entries_8", |b| {
        b.iter(|| {
            let _ = Ring::new(8).expect("Ring::new");
        });
    });
    group.bench_function("entries_64", |b| {
        b.iter(|| {
            let _ = Ring::new(64).expect("Ring::new");
        });
    });
    group.finish();
}

#[cfg(target_os = "linux")]
criterion_group!(benches, bench_pipe_drain, bench_ring_new);

#[cfg(not(target_os = "linux"))]
fn bench_compare_uring_stub(c: &mut Criterion) {
    c.bench_function("compare_uring_stub_non_linux", |b| b.iter(|| {}));
}

#[cfg(not(target_os = "linux"))]
criterion_group!(benches, bench_compare_uring_stub);

criterion_main!(benches);
