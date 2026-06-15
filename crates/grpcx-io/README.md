# grpcx-io

[![crates.io](https://img.shields.io/crates/v/grpcx-io.svg)](https://crates.io/crates/grpcx-io)
[![docs.rs](https://docs.rs/grpcx-io/badge.svg)](https://docs.rs/grpcx-io)
[![license](https://img.shields.io/crates/l/grpcx-io.svg)](https://github.com/goliajp/grpcx)

Zero-dep cross-platform async-ready OS IO abstraction —
`epoll` / `io_uring` / `kqueue` under one unified `Poll` + a sibling
`Ring` for io_uring's completion model. **Not a runtime**, just the
readiness primitive layer.

## Status (v1.1 — L1 released + cross-thread Waker)

Linux `epoll` + `io_uring`, Darwin `kqueue`, FreeBSD `kqueue` all live
and semver-stable. At parity-or-better with `mio` on every measured
bench across 3 OSes; the io_uring lane is **1.29× faster than `mio`
epoll** at N ∈ {8, 64, 256}. Cross-OS numbers in
[../../PERFORMANCE.md](../../PERFORMANCE.md). Per-path budgets in
[BUDGETS.md](./BUDGETS.md). Windows IOCP removed from v1 scope — no
validation hardware (see
[../../REFACTOR-v1-v0.6-windows-iocp.md](../../REFACTOR-v1-v0.6-windows-iocp.md)).

**v1.1** adds [`Waker`] — a cross-thread reactor wakeup (`mio::Waker`
parity): `eventfd` on Linux, `kqueue` `EVFILT_USER` on the BSDs. Lets a
runtime unpark a blocked `poll` from another thread. See
[../../REFACTOR-v1.1-waker.md](../../REFACTOR-v1.1-waker.md).

[`Waker`]: https://docs.rs/grpcx-io/latest/grpcx_io/struct.Waker.html

## Quickstart — readiness `Poll`

```rust,no_run
use grpcx_io::{Events, Interest, IoResult, Poll, Token};

fn main() -> IoResult<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        use grpcx_io::SourceFd;
        let mut source = SourceFd::new(0);
        poll.registry()
            .register(&mut source, Token(0), Interest::READABLE)?;
    }

    poll.poll(&mut events, None)?;
    for event in &events {
        let _ = event.token();
        let _ = event.ready();
    }
    Ok(())
}
```

## Quickstart — io_uring `Ring` (Linux only)

```rust,no_run
# #[cfg(target_os = "linux")] fn run() -> grpcx_io::IoResult<()> {
use grpcx_io::{Ring, Sqe};

let mut ring = Ring::new(64)?;
let nop = Sqe::nop(0xc0ffee); // user_data round-trips into the Cqe
ring.push_sqe(&nop)?;
ring.submit_and_wait(1, 1)?;
ring.reap_cqes(|cqe| {
    assert_eq!(cqe.user_data, 0xc0ffee);
});
# Ok(()) }
```

`Ring` is **completion-based**, not a substitute for `Poll`. Use
`Poll` when the workload is "wait for which fds are ready then do
syscalls on them"; use `Ring` when the workload is "submit a batch
of ops and harvest completions". Mixing both in one program is fine.

## When to reach for this vs `mio`

| You want | `grpcx-io` | `mio` |
|---|---|---|
| Zero-dep stack with everything in-house | ✓ | ✗ (`libc` + `windows-sys`) |
| Stable, battle-tested ecosystem | not yet — v0.8 | ✓ |
| `io_uring` as a first-class backend (not a substitute, a sibling) | ✓ (v0.4+) | ✗ |
| One API across Linux + Darwin + FreeBSD (Windows post-v1) | ✓ | ✓ |
| `no_std` compatible (host-OS targets) | ✓ | ✗ |
| Inline syscall layer, no `libc` even transitively | ✓ | ✗ |

## Cross-OS bench (v0.8 sustained criterion)

| Op | Linux (lx64) | Darwin (mac) | FreeBSD (KVM) | vs `mio` |
|---|---:|---:|---:|---|
| `poll_idle` empty | 90.6 ns | 12.5 µs | 124.6 ns | FreeBSD **+7% faster** |
| `register+dereg` | 506 ns | 584 ns | 451 ns | parity |
| `reregister` only | 128 ns | 322 ns | 297 ns | FreeBSD **+4% faster** |
| `poll_drain` N=256 | 114.4 µs | 229.9 µs | 77.5 µs | parity |
| `Ring` (io_uring) N=256 | 88.9 µs | — | — | **1.29× faster than mio epoll** |

Full table + reproducibility commands in
[../../PERFORMANCE.md](../../PERFORMANCE.md).

## Design philosophy

The crate is part of the bigger
[grpcx](../../README.md) project. Read in order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md) — stones / cement / layer
   discipline / priority axes
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md) — why every layer must be
   rewritten + the L3 crypto wrapper exception
3. [ROADMAP.md](../../ROADMAP.md) — L1 → L10 with per-layer triggers

Per-checkpoint design notes:
[REFACTOR-v1-v0.1-bootstrap.md](../../REFACTOR-v1-v0.1-bootstrap.md),
[v0.2 (epoll)](../../REFACTOR-v1-v0.2-epoll.md),
[v0.3 (bench harness)](../../REFACTOR-v1-v0.3-bench-harness.md),
[v0.4 (io_uring)](../../REFACTOR-v1-v0.4-io_uring.md),
[v0.5 (Darwin kqueue)](../../REFACTOR-v1-v0.5-kqueue.md),
[v0.6 (Windows IOCP — parked)](../../REFACTOR-v1-v0.6-windows-iocp.md),
[v0.7 (FreeBSD kqueue)](../../REFACTOR-v1-v0.7-freebsd-kqueue.md),
[v0.8 (bench publish)](../../REFACTOR-v1-v0.8-bench-publish.md).

## License

Apache-2.0 OR MIT
