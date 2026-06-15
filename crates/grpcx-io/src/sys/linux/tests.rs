//! Conformance tests for the Linux `epoll` backend.
//!
//! Lives under `sys::linux::tests` (not `tests/`) so it can reach
//! `pub(crate)` helpers like `raw::pipe2` / `raw::write` without
//! widening the crate's public surface.

use super::raw;
use crate::{Events, Interest, IoErrorKind, Poll, Ready, SourceFd, Token};
use core::time::Duration;

/// Register → reregister → deregister round-trip; a second
/// deregister surfaces `NotFound` (kernel `ENOENT`).
#[test]
fn lifecycle_register_reregister_deregister() {
    let poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);

    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(42), Interest::READABLE)
        .unwrap();
    registry
        .reregister(&mut src, Token(43), Interest::READABLE)
        .unwrap();
    registry.deregister(&mut src).unwrap();

    let r = registry.deregister(&mut src);
    assert!(r.is_err(), "second deregister should fail");
    assert_eq!(r.unwrap_err().kind(), IoErrorKind::NotFound);

    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// Writing to a pipe's write end makes the read end `READABLE`.
#[test]
fn pipe_write_observes_readable() {
    let mut poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);

    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(1), Interest::READABLE)
        .unwrap();

    let n = raw::write(wfd, b"x").unwrap();
    assert_eq!(n, 1);

    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::from_millis(500)))
        .unwrap();

    assert_eq!(events.len(), 1, "expected exactly one ready event");
    let ev = events.iter().next().unwrap();
    assert_eq!(ev.token(), Token(1));
    assert!(
        ev.ready().contains(Ready::READABLE),
        "ready = {:?}",
        ev.ready()
    );

    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// `poll` with a zero timeout returns immediately with an empty
/// event set when nothing is ready.
#[test]
fn poll_zero_timeout_returns_empty() {
    let mut poll = Poll::new().unwrap();
    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::ZERO)).unwrap();
    assert!(events.is_empty());
}

/// `poll` with a short timeout returns empty if no readiness fires
/// during the wait.
#[test]
fn poll_short_timeout_returns_empty() {
    let mut poll = Poll::new().unwrap();
    let mut events = Events::with_capacity(16);
    poll.poll(&mut events, Some(Duration::from_millis(10)))
        .unwrap();
    assert!(events.is_empty());
}

/// Drive the `__bench_helpers` public shim (used by `benches/`) so
/// coverage accounts for every function it re-exports.
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

/// `poll` indefinite wait — set up a write before poll so it
/// resolves quickly. Exercises the `timeout = None` arm of
/// `duration_to_ms`.
#[test]
fn poll_indefinite_wait_resolves_on_write() {
    let mut poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);
    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(2), Interest::READABLE)
        .unwrap();

    raw::write(wfd, b"z").unwrap();

    let mut events = Events::with_capacity(8);
    poll.poll(&mut events, None).unwrap();
    assert_eq!(events.len(), 1);

    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// Register `WRITABLE` and `PRIORITY` interest variants exercise
/// `interest_to_epoll_events`'s `EPOLLOUT` / `EPOLLPRI` arms.
#[test]
fn register_writable_and_priority_interests() {
    let poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);
    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(10), Interest::WRITABLE | Interest::PRIORITY)
        .unwrap();
    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// `for ev in &events` exercises the `IntoIterator for &Events` impl.
#[test]
fn events_into_iterator_via_ref() {
    let mut poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);
    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(20), Interest::READABLE)
        .unwrap();
    raw::write(wfd, b"i").unwrap();

    let mut events = Events::with_capacity(4);
    poll.poll(&mut events, Some(Duration::from_millis(100)))
        .unwrap();
    let mut seen = 0usize;
    for ev in &events {
        let _ = ev.token();
        seen += 1;
    }
    assert!(seen >= 1);

    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// Registering the same fd twice trips kernel `EEXIST` →
/// `IoErrorKind::AlreadyRegistered` — covers raw.rs errno arm 17.
#[test]
fn double_register_yields_already_registered() {
    let poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);
    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(30), Interest::READABLE)
        .unwrap();
    let r = registry.register(&mut src, Token(31), Interest::READABLE);
    assert!(r.is_err(), "second register should fail");
    assert_eq!(r.unwrap_err().kind(), IoErrorKind::AlreadyRegistered);

    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}

/// Reading from a deliberately invalid fd surfaces `IoErrorKind::
/// InvalidInput` (kernel `EBADF`) — covers raw.rs errno arm 9.
#[test]
fn read_invalid_fd_yields_invalid_input() {
    let mut buf = [0u8; 8];
    // -1 / large unused number — guaranteed not allocated.
    let r = raw::read(-1, &mut buf);
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().kind(), IoErrorKind::InvalidInput);
}

/// Saturated `Duration` (≥ `i32::MAX` ms) clamps to `i32::MAX` ms
/// in `duration_to_ms` — exercise the clamp branch.
#[test]
fn poll_saturated_timeout_clamps_not_panics() {
    let mut poll = Poll::new().unwrap();
    let (rfd, wfd) = raw::pipe2(raw::O_CLOEXEC).unwrap();
    let mut src = SourceFd::new(rfd);
    let registry = poll.registry().clone();
    registry
        .register(&mut src, Token(3), Interest::READABLE)
        .unwrap();

    // Write FIRST so poll returns immediately even though the
    // requested timeout is enormous.
    raw::write(wfd, b"q").unwrap();
    let mut events = Events::with_capacity(8);
    // u64::MAX ms ~ 584M years. Should clamp to i32::MAX ms and
    // then return immediately since data is ready.
    poll.poll(&mut events, Some(Duration::from_secs(u64::MAX / 1000)))
        .unwrap();
    assert!(!events.is_empty());

    registry.deregister(&mut src).unwrap();
    let _ = raw::close(rfd);
    let _ = raw::close(wfd);
}
