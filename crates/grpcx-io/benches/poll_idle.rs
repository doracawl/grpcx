//! `Poll::poll(Some(Duration::ZERO))` empty-wakeup steady-state cost.
//!
//! With no sources registered and a zero timeout, the dominant cost
//! is a single `epoll_pwait` syscall returning immediately with no
//! events. The bench measures one full round-trip through the
//! reactor — buffer reset, syscall, translation loop (zero items),
//! return.

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn bench_poll_idle(c: &mut Criterion) {
    use core::time::Duration;
    use grpcx_io::{Events, Poll};

    let mut poll = Poll::new().expect("Poll::new");
    let mut events = Events::with_capacity(64);
    let timeout = Some(Duration::ZERO);

    c.bench_function("poll_idle_zero_timeout", |b| {
        b.iter(|| {
            poll.poll(&mut events, timeout).expect("poll");
        });
    });
}

#[cfg(not(target_os = "linux"))]
fn bench_poll_idle(c: &mut Criterion) {
    // grpcx-io has no backend on this target as of v0.3; the bench
    // exists only so `cargo bench --workspace` does not fail to build
    // the bench target on non-Linux hosts.
    c.bench_function("poll_idle_stub_non_linux", |b| b.iter(|| {}));
}

criterion_group!(benches, bench_poll_idle);
criterion_main!(benches);
