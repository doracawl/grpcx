//! `register` / `deregister` and `reregister` hot-path latency on a
//! single pre-created pipe fd. Excludes pipe-creation cost from the
//! measurement.

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn bench_register_fastpath(c: &mut Criterion) {
    use grpcx_io::__bench_helpers::{O_CLOEXEC, close, pipe2};
    use grpcx_io::{Interest, Poll, SourceFd, Token};

    let poll = Poll::new().expect("Poll::new");
    let registry = poll.registry().clone();
    let (rfd, wfd) = pipe2(O_CLOEXEC).expect("pipe2");
    let mut src = SourceFd::new(rfd);

    let mut group = c.benchmark_group("register_fastpath");

    group.bench_function("register_then_deregister", |b| {
        b.iter(|| {
            registry
                .register(&mut src, Token(0), Interest::READABLE)
                .expect("register");
            registry.deregister(&mut src).expect("deregister");
        });
    });

    group.bench_function("reregister_only", |b| {
        registry
            .register(&mut src, Token(0), Interest::READABLE)
            .expect("register");
        b.iter(|| {
            registry
                .reregister(&mut src, Token(0), Interest::READABLE | Interest::WRITABLE)
                .expect("reregister");
        });
        registry.deregister(&mut src).expect("deregister");
    });

    group.finish();

    let _ = close(rfd);
    let _ = close(wfd);
}

#[cfg(not(target_os = "linux"))]
fn bench_register_fastpath(c: &mut Criterion) {
    c.bench_function("register_fastpath_stub_non_linux", |b| b.iter(|| {}));
}

criterion_group!(benches, bench_register_fastpath);
criterion_main!(benches);
