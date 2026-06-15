//! Register `N` pipe-read endpoints, fire all on each iteration,
//! poll + drain. Measures per-event amortized round-trip cost.
//!
//! `N = 1, 8, 64, 256`. Higher N (e.g. 1024) is gated by the host's
//! `ulimit -n`; defer until the harness can raise it on demand.

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn bench_poll_drain(c: &mut Criterion) {
    use core::time::Duration;
    use criterion::BenchmarkId;
    use grpcx_io::__bench_helpers::{O_CLOEXEC, RawFd, close, pipe2, read, write};
    use grpcx_io::{Events, Interest, Poll, SourceFd, Token};

    let mut group = c.benchmark_group("poll_drain");

    for &n in &[1usize, 8, 64, 256] {
        let mut poll = Poll::new().expect("Poll::new");
        let registry = poll.registry().clone();
        let mut events = Events::with_capacity(n.max(64));

        let mut sources: Vec<SourceFd> = Vec::with_capacity(n);
        let mut writes: Vec<RawFd> = Vec::with_capacity(n);
        let mut reads: Vec<RawFd> = Vec::with_capacity(n);
        for i in 0..n {
            let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
            let mut src = SourceFd::new(rfd);
            registry
                .register(&mut src, Token(i), Interest::READABLE)
                .expect("register");
            sources.push(src);
            writes.push(wfd);
            reads.push(rfd);
        }

        let mut scratch = [0u8; 64];

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Fire readiness on every source.
                for &wfd in &writes {
                    write(wfd, b"x").expect("write");
                }
                // Drain the reactor.
                poll.poll(&mut events, Some(Duration::from_millis(100)))
                    .expect("poll");
                // Drain the pipes so the kernel buffer doesn't fill.
                for &rfd in &reads {
                    let _ = read(rfd, &mut scratch);
                }
            });
        });

        // Cleanup.
        for mut src in sources.drain(..) {
            registry.deregister(&mut src).ok();
        }
        for fd in reads.drain(..) {
            let _ = close(fd);
        }
        for fd in writes.drain(..) {
            let _ = close(fd);
        }
    }
    group.finish();
}

#[cfg(not(target_os = "linux"))]
fn bench_poll_drain(c: &mut Criterion) {
    c.bench_function("poll_drain_stub_non_linux", |b| b.iter(|| {}));
}

criterion_group!(benches, bench_poll_drain);
criterion_main!(benches);
