//! Conformance tests for the Darwin syscall layer.
//!
//! T24 starts with a single round-trip smoke that exercises every
//! piece of the inline-asm ABI: `pipe_cloexec` (special two-fd
//! return-pair shape + carry-flag error path), `write` (syscall3),
//! `read` (syscall3 writing back into the caller's buffer), and
//! `close`. If this passes the syscall layer is fundamentally
//! correct; the full Linux-mirror suite lands in T28.

use super::kqueue::{Selector, kevent_to_ready, token_from_event};
use super::raw;
use crate::{Events, Interest, Poll, Ready, SourceFd, Token};
use core::time::Duration;

/// `kqueue()` returns a valid fd and `close()` releases it.
#[test]
fn kqueue_create_close_roundtrip() {
    let kq = raw::kqueue().expect("kqueue");
    raw::close(kq).expect("close");
}

/// `kevent()` with an empty change list and a zero timeout returns
/// 0 events. Validates `syscall6` (six-arg ABI) and the
/// [`raw::Kevent`] / [`raw::Timespec`] layouts the kernel writes
/// through.
#[test]
fn kevent_empty_call_returns_zero() {
    let kq = raw::kqueue().expect("kqueue");
    let mut events: [raw::Kevent; 1] = [Default::default()];
    let timeout = raw::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let n = raw::kevent(kq, &[], &mut events, Some(&timeout)).expect("kevent");
    assert_eq!(n, 0);
    raw::close(kq).expect("close");
}

/// Round-trip a payload through a `pipe()` pair built via Darwin's
/// register-pair return convention. Touches every code path in the
/// raw syscall layer.
#[test]
fn pipe_write_read_roundtrip() {
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");

    let payload = b"hello darwin";
    let written = raw::write(w, payload).expect("write");
    assert_eq!(written, payload.len());

    let mut buf = [0u8; 32];
    let n = raw::read(r, &mut buf).expect("read");
    assert_eq!(n, payload.len());
    assert_eq!(&buf[..n], payload);

    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// Register a pipe's read end with the Selector, write a byte to
/// the write end, kevent-wait, observe one READABLE event whose
/// `udata`-derived token matches what we registered.
#[test]
fn selector_pipe_write_observes_readable() {
    let sel = Selector::new().expect("Selector::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    sel.register(r, Token(7), Interest::READABLE)
        .expect("register");

    raw::write(w, b"x").expect("write");

    let mut events: [raw::Kevent; 4] = [Default::default(); 4];
    let timeout = raw::Timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    let n = sel.wait(&mut events, Some(timeout)).expect("wait");
    assert!(n >= 1, "expected at least one event, got {n}");

    let ev = &events[0];
    assert_eq!(token_from_event(ev), Token(7));
    assert!(
        kevent_to_ready(ev).contains(Ready::READABLE),
        "ready = {:?}",
        kevent_to_ready(ev),
    );

    sel.deregister(r).expect("deregister");
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// Deregistering an fd that was never registered surfaces
/// `NotFound`, matching the v0.2 epoll contract for cross-OS
/// consistency.
#[test]
fn selector_deregister_unregistered_returns_notfound() {
    let sel = Selector::new().expect("Selector::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    let err = sel
        .deregister(r)
        .expect_err("deregister of an unregistered fd must error");
    assert_eq!(err.kind(), crate::IoErrorKind::NotFound);
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// Lifecycle round-trip via the public `Registry`: register →
/// reregister → deregister → second deregister fails with
/// `NotFound`. Mirrors the v0.2 epoll suite.
#[test]
fn lifecycle_register_reregister_deregister() {
    let poll = Poll::new().expect("Poll::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    let mut src = SourceFd::new(r);
    let registry = poll.registry().clone();

    registry
        .register(&mut src, Token(42), Interest::READABLE)
        .expect("register");
    registry
        .reregister(&mut src, Token(43), Interest::READABLE)
        .expect("reregister");
    registry.deregister(&mut src).expect("first deregister");

    let err = registry
        .deregister(&mut src)
        .expect_err("second deregister must fail");
    assert_eq!(err.kind(), crate::IoErrorKind::NotFound);

    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// `poll` with `Duration::ZERO` returns immediately with an empty
/// event set when nothing is ready.
#[test]
fn poll_zero_timeout_returns_empty() {
    let mut poll = Poll::new().expect("Poll::new");
    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::ZERO)).expect("poll");
    assert!(events.is_empty());
}

/// `poll` with a short timeout returns empty if no readiness fires
/// during the wait.
#[test]
fn poll_short_timeout_returns_empty() {
    let mut poll = Poll::new().expect("Poll::new");
    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::from_millis(10)))
        .expect("poll");
    assert!(events.is_empty());
}

/// End-to-end check via the public `Poll` / `Registry` / `SourceFd`
/// surface — proves the T27 cfg wiring routes a Darwin build into
/// `kqueue::Selector` without bypassing any layer.
#[test]
fn public_poll_pipe_write_observes_readable() {
    let mut poll = Poll::new().expect("Poll::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    let mut src = SourceFd::new(r);

    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(11), Interest::READABLE)
        .expect("register via public Registry");

    let n = raw::write(w, b"x").expect("write");
    assert_eq!(n, 1);

    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::from_secs(1)))
        .expect("Poll::poll");

    assert_eq!(events.len(), 1, "expected exactly one ready event");
    let ev = events.iter().next().unwrap();
    assert_eq!(ev.token(), Token(11));
    assert!(
        ev.ready().contains(Ready::READABLE),
        "ready = {:?}",
        ev.ready(),
    );

    registry.deregister(&mut src).expect("deregister");
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// Drive the `__bench_helpers` public shim so coverage accounts for
/// every function it re-exports.
#[test]
fn bench_helpers_roundtrip_pipe2_write_read_close() {
    use crate::__bench_helpers;
    let (r, w) = __bench_helpers::pipe2(__bench_helpers::O_CLOEXEC).expect("pipe2");
    let n_w = __bench_helpers::write(w, b"y").expect("write");
    assert_eq!(n_w, 1);
    let mut buf = [0u8; 1];
    let n_r = __bench_helpers::read(r, &mut buf).expect("read");
    assert_eq!(n_r, 1);
    assert_eq!(buf[0], b'y');
    __bench_helpers::close(r).expect("close r");
    __bench_helpers::close(w).expect("close w");
}

/// Build a registration with all three Darwin filter bits
/// (`READABLE | WRITABLE | PRIORITY`) so `interest_to_changes`
/// lays down `EVFILT_READ` + `EVFILT_WRITE` + `EVFILT_EXCEPT`
/// in the same change list. The kernel rejects `EVFILT_EXCEPT`
/// on pipes (it's a socket-OOB filter) with `EINVAL`; the goal
/// of this test is to drive the **build** side of the change
/// list, not to succeed at registration.
#[test]
fn interest_all_three_arms_build_change_list_without_panic() {
    let sel = Selector::new().expect("Selector::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    // Don't .expect() — pipe-fd + EVFILT_EXCEPT is EINVAL by
    // design; we just want the code-path through
    // interest_to_changes covered.
    let _ = sel.register(
        r,
        Token(99),
        Interest::READABLE | Interest::WRITABLE | Interest::PRIORITY,
    );
    // Best-effort cleanup; kqueue rejects partial deregister
    // gracefully via the all-ENOENT → NotFound path.
    let _ = sel.deregister(r);
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// Reregister narrowing from `READABLE | WRITABLE` → `READABLE`
/// triggers the complement-DEL fast path: ADD READ (idempotent),
/// DEL WRITE, DEL EXCEPT. Single `kevent` syscall. (Skips PRIORITY
/// in the wide form so the initial register actually succeeds on
/// a pipe fd.)
#[test]
fn selector_reregister_narrow_drops_filters() {
    let sel = Selector::new().expect("Selector::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");
    sel.register(r, Token(50), Interest::READABLE | Interest::WRITABLE)
        .expect("register wide");
    sel.reregister(r, Token(51), Interest::READABLE)
        .expect("reregister narrow to READABLE");
    raw::write(w, b"q").expect("write");

    let mut events: [raw::Kevent; 4] = [Default::default(); 4];
    let timeout = raw::Timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    let n = sel.wait(&mut events, Some(timeout)).expect("wait");
    assert!(n >= 1);
    let ev = &events[0];
    assert_eq!(token_from_event(ev), Token(51));
    assert!(kevent_to_ready(ev).contains(Ready::READABLE));

    sel.deregister(r).expect("deregister");
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}

/// `read` on a deliberately invalid fd (`-1`) surfaces an error,
/// exercising the carry-flag error path of `raw::read`.
#[test]
fn read_invalid_fd_returns_error() {
    let mut buf = [0u8; 4];
    assert!(raw::read(-1, &mut buf).is_err());
}

/// Reregister broadens / narrows the interest set without leaking
/// the old subscription.
#[test]
fn selector_reregister_changes_interest() {
    let sel = Selector::new().expect("Selector::new");
    let (r, w) = raw::pipe_cloexec().expect("pipe_cloexec");

    sel.register(r, Token(1), Interest::WRITABLE)
        .expect("initial register");
    sel.reregister(r, Token(2), Interest::READABLE)
        .expect("reregister narrowing to READABLE");

    raw::write(w, b"x").expect("write");

    let mut events: [raw::Kevent; 4] = [Default::default(); 4];
    let timeout = raw::Timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    let n = sel.wait(&mut events, Some(timeout)).expect("wait");
    assert!(n >= 1);

    let ev = &events[0];
    assert_eq!(
        token_from_event(ev),
        Token(2),
        "token should reflect the reregister"
    );
    assert!(kevent_to_ready(ev).contains(Ready::READABLE));

    sel.deregister(r).expect("deregister");
    raw::close(r).expect("close r");
    raw::close(w).expect("close w");
}
