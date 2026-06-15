//! Head-to-head bench: `grpcx-io` vs `mio` on identical workloads.
//!
//! Both sides drive the same kernel through their respective
//! reactors. Fairness invariants:
//!
//! - Same workload shape (same number of fds, same readiness type).
//! - Both use the same `pipe2` syscall for fd creation (via
//!   `grpcx_io::__bench_helpers`) so neither side gains an
//!   instantiation-cost difference.
//! - Both pass `Some(Duration::ZERO)` to isolate the empty-wait
//!   round-trip from `epoll_pwait`'s blocking path.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_poll_idle(c: &mut Criterion) {
    use core::time::Duration;

    let mut group = c.benchmark_group("compare_mio/poll_idle_zero_timeout");

    // grpcx-io
    {
        let mut poll = grpcx_io::Poll::new().expect("grpcx Poll");
        let mut events = grpcx_io::Events::with_capacity(64);
        group.bench_function("grpcx_io", |b| {
            b.iter(|| {
                poll.poll(&mut events, Some(Duration::ZERO))
                    .expect("grpcx poll");
            });
        });
    }

    // mio
    {
        let mut poll = mio::Poll::new().expect("mio Poll");
        let mut events = mio::Events::with_capacity(64);
        group.bench_function("mio", |b| {
            b.iter(|| {
                poll.poll(&mut events, Some(Duration::ZERO))
                    .expect("mio poll");
            });
        });
    }

    group.finish();
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_register_fastpath(c: &mut Criterion) {
    use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2};

    let mut group = c.benchmark_group("compare_mio/register_then_dereg");

    // grpcx-io
    {
        use grpcx_io::{Interest, Poll, SourceFd, Token};

        let poll = Poll::new().expect("grpcx Poll");
        let registry = poll.registry().clone();
        let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
        let mut src = SourceFd::new(rfd);

        group.bench_function("grpcx_io", |b| {
            b.iter(|| {
                registry
                    .register(&mut src, Token(0), Interest::READABLE)
                    .expect("register");
                registry.deregister(&mut src).expect("deregister");
            });
        });

        let _ = close(rfd);
        let _ = close(wfd);
    }

    // mio
    {
        use mio::unix::SourceFd as MioSourceFd;
        use mio::{Interest, Poll, Token};

        let poll = Poll::new().expect("mio Poll");
        let registry = poll
            .registry()
            .try_clone()
            .expect("mio Registry::try_clone");
        let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
        let mut src = MioSourceFd(&rfd);

        group.bench_function("mio", |b| {
            b.iter(|| {
                registry
                    .register(&mut src, Token(0), Interest::READABLE)
                    .expect("register");
                registry.deregister(&mut src).expect("deregister");
            });
        });

        let _ = close(rfd);
        let _ = close(wfd);
    }

    group.finish();
}

/// Reregister-only bench: register once outside the timed region;
/// every iteration calls `reregister` to a new token+interest pair.
/// Isolates the reregister fast path from `register + deregister`.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_reregister(c: &mut Criterion) {
    use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2};

    let mut group = c.benchmark_group("compare_mio/reregister_only");

    // grpcx-io
    {
        use grpcx_io::{Interest, Poll, SourceFd, Token};

        let poll = Poll::new().expect("grpcx Poll");
        let registry = poll.registry().clone();
        let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
        let mut src = SourceFd::new(rfd);
        registry
            .register(&mut src, Token(0), Interest::READABLE)
            .expect("initial register");

        let mut tick: usize = 0;
        group.bench_function("grpcx_io", |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                // Alternate READABLE / WRITABLE to force a real
                // kernel-side state change, not a noop.
                let interest = if tick & 1 == 0 {
                    Interest::READABLE
                } else {
                    Interest::WRITABLE
                };
                registry
                    .reregister(&mut src, Token(tick), interest)
                    .expect("reregister");
            });
        });

        let _ = registry.deregister(&mut src);
        let _ = close(rfd);
        let _ = close(wfd);
    }

    // mio
    {
        use mio::unix::SourceFd as MioSourceFd;
        use mio::{Interest, Poll, Token};

        let poll = Poll::new().expect("mio Poll");
        let registry = poll
            .registry()
            .try_clone()
            .expect("mio Registry::try_clone");
        let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
        let mut src = MioSourceFd(&rfd);
        registry
            .register(&mut src, Token(0), Interest::READABLE)
            .expect("initial register");

        let mut tick: usize = 0;
        group.bench_function("mio", |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                let interest = if tick & 1 == 0 {
                    Interest::READABLE
                } else {
                    Interest::WRITABLE
                };
                registry
                    .reregister(&mut src, Token(tick), interest)
                    .expect("reregister");
            });
        });

        let _ = registry.deregister(&mut src);
        let _ = close(rfd);
        let _ = close(wfd);
    }

    group.finish();
}

/// Poll drain bench: N pipes registered READABLE, one byte written
/// to each `w` end before every iteration; the timed region is
/// "wait for readiness + iterate the events". Parameterized over
/// N to surface scaling behavior.
///
/// Per-iteration work is `N writes + 1 poll syscall + N event
/// translates`. The `N writes` are done inside the timed region
/// (criterion's per-iter setup-isolation is not free; including
/// the writes is fairer than excluding them since both sides do
/// the same write loop).
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_poll_drain(c: &mut Criterion) {
    use core::time::Duration;
    use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2, write};

    let mut group = c.benchmark_group("compare_mio/poll_drain");

    for &n in &[8usize, 64, 256] {
        group.throughput(Throughput::Elements(n as u64));

        // grpcx-io
        {
            use grpcx_io::{Events, Interest, Poll, SourceFd, Token};

            let mut poll = Poll::new().expect("grpcx Poll");
            let registry = poll.registry().clone();
            let pipes: Vec<(i32, i32)> = (0..n).map(|_| pipe2(O_CLOEXEC).expect("pipe2")).collect();
            let mut srcs: Vec<SourceFd> = pipes.iter().map(|&(r, _)| SourceFd::new(r)).collect();
            for (i, src) in srcs.iter_mut().enumerate() {
                registry
                    .register(src, Token(i), Interest::READABLE)
                    .expect("register");
            }
            let mut events = Events::with_capacity(n);

            group.bench_with_input(BenchmarkId::new("grpcx_io", n), &(), |b, _| {
                b.iter(|| {
                    for &(_, w) in &pipes {
                        let _ = write(w, b"x");
                    }
                    poll.poll(&mut events, Some(Duration::from_secs(1)))
                        .expect("grpcx poll");
                    let mut seen = 0usize;
                    for _ in &events {
                        seen += 1;
                    }
                    assert!(seen >= n, "expected {} events, drained {}", n, seen);
                    // Drain the bytes so the next iter re-arms
                    // edge-triggered filters.
                    let mut buf = [0u8; 1];
                    for &(r, _) in &pipes {
                        let _ = grpcx_io::__bench_helpers::read(r, &mut buf);
                    }
                });
            });

            for &(r, w) in &pipes {
                let _ = close(r);
                let _ = close(w);
            }
        }

        // mio
        {
            use mio::unix::SourceFd as MioSourceFd;
            use mio::{Events, Interest, Poll, Token};

            let mut poll = Poll::new().expect("mio Poll");
            let registry = poll
                .registry()
                .try_clone()
                .expect("mio Registry::try_clone");
            let pipes: Vec<(i32, i32)> = (0..n).map(|_| pipe2(O_CLOEXEC).expect("pipe2")).collect();
            for (i, &(r, _)) in pipes.iter().enumerate() {
                registry
                    .register(&mut MioSourceFd(&r), Token(i), Interest::READABLE)
                    .expect("register");
            }
            let mut events = Events::with_capacity(n);

            group.bench_with_input(BenchmarkId::new("mio", n), &(), |b, _| {
                b.iter(|| {
                    for &(_, w) in &pipes {
                        let _ = write(w, b"x");
                    }
                    poll.poll(&mut events, Some(Duration::from_secs(1)))
                        .expect("mio poll");
                    let mut seen = 0usize;
                    for _ in events.iter() {
                        seen += 1;
                    }
                    assert!(seen >= n, "expected {} events, drained {}", n, seen);
                    let mut buf = [0u8; 1];
                    for &(r, _) in &pipes {
                        let _ = grpcx_io::__bench_helpers::read(r, &mut buf);
                    }
                });
            });

            for &(r, w) in &pipes {
                let _ = close(r);
                let _ = close(w);
            }
        }
    }

    group.finish();
}

/// Allocation/init cost: `Poll::new()`. One per worker thread —
/// runs at startup, not in the hot path. Bench it to fill the
/// BUDGETS table's WARM tier row.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_poll_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_mio/poll_new");

    group.bench_function("grpcx_io", |b| {
        b.iter(|| {
            let _ = grpcx_io::Poll::new().expect("grpcx Poll::new");
        });
    });

    group.bench_function("mio", |b| {
        b.iter(|| {
            let _ = mio::Poll::new().expect("mio Poll::new");
        });
    });

    group.finish();
}

/// Allocation cost: `Events::with_capacity(64)`. WARM tier — the
/// `Events` buffer is reused across many polls, this is paid
/// once per `Events` instance.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_compare_events_with_capacity(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_mio/events_with_capacity_64");

    group.bench_function("grpcx_io", |b| {
        b.iter(|| {
            let _ = grpcx_io::Events::with_capacity(64);
        });
    });

    group.bench_function("mio", |b| {
        b.iter(|| {
            let _ = mio::Events::with_capacity(64);
        });
    });

    group.finish();
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
criterion_group!(
    benches,
    bench_compare_poll_idle,
    bench_compare_register_fastpath,
    bench_compare_reregister,
    bench_compare_poll_drain,
    bench_compare_poll_new,
    bench_compare_events_with_capacity,
    bench_event_translate,
);

/// Per-event translate microbench: feed a fixed kernel-shaped byte
/// buffer through the per-OS decoder (epoll_event → `Event` on
/// Linux, kevent → `Event` on Darwin/iOS/FreeBSD) and measure the
/// per-call cost in isolation from any syscall. Uses the
/// `__fuzz_helpers` doc-hidden surface so the bench reaches the
/// `pub(crate)` translation funcs.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn bench_event_translate(c: &mut Criterion) {
    use grpcx_io::__fuzz_helpers::decode_one_event;

    let mut group = c.benchmark_group("event_translate");

    // 64 bytes covers every backend's event struct size; the decoder
    // only reads the prefix it needs. Bits arranged so the
    // translation produces a non-empty `Ready` on every backend.
    let raw_bytes: [u8; 64] = {
        let mut b = [0u8; 64];
        // First byte → "READABLE" bit on most backends (varies
        // by struct layout but criterion only needs a stable
        // input across iters).
        b[0] = 0x01;
        b[8] = 0xff;
        b[16] = 0x01;
        b
    };

    group.bench_function("decode_one_event", |b| {
        b.iter(|| {
            let _ = decode_one_event(&raw_bytes);
        });
    });

    group.finish();
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
fn bench_compare_stub(c: &mut Criterion) {
    c.bench_function("compare_mio_stub_no_backend", |b| b.iter(|| {}));
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
criterion_group!(benches, bench_compare_stub);

criterion_main!(benches);
