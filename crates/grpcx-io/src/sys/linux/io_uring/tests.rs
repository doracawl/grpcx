//! Conformance tests for the Linux `io_uring` backend.
//!
//! Lives under `sys::linux::io_uring::tests` (not `tests/`) so it can
//! reach `pub(crate)` syscall helpers in `raw` directly.
//!
//! These tests touch the real `io_uring` syscalls and therefore only
//! compile + run on Linux. macOS hosts skip them entirely.

use super::{Cqe, Ring, Sqe};
use crate::sys::linux::raw::{O_CLOEXEC, close, pipe2, write};
use crate::types::IoErrorKind;

/// `Ring::new(8)` succeeds on any kernel that supports io_uring.
/// Kernels < 5.1 return `BackendUnavailable`; CI tracks kernel
/// version separately and skips this suite on too-old hosts.
#[test]
fn ring_new_succeeds() {
    let _ring = Ring::new(8).expect("Ring::new should succeed on a >= 5.1 kernel");
}

/// Round-trip a `NOP` SQE: push → submit_and_wait(1, 1) → reap one
/// CQE with matching `user_data` and `res = 0`.
#[test]
fn nop_round_trip() {
    let mut ring = Ring::new(8).expect("ring");
    let sqe = Sqe::nop(0xdead_beef);
    ring.push_sqe(&sqe).expect("push_sqe");

    let submitted = ring.submit_and_wait(1, 1).expect("submit_and_wait");
    assert_eq!(submitted, 1, "kernel should consume exactly 1 SQE");

    let mut got: Option<Cqe> = None;
    let drained = ring.reap_cqes(|cqe| got = Some(cqe));
    assert_eq!(drained, 1, "expected exactly one CQE");

    let cqe = got.expect("cqe was captured");
    assert_eq!(cqe.user_data, 0xdead_beef);
    assert_eq!(cqe.res, 0, "NOP completes with res = 0");
}

/// `read` on a pipe whose write end already has data should
/// complete with the byte count and copy the bytes into the buffer.
#[test]
fn read_pipe_round_trip() {
    let mut ring = Ring::new(8).expect("ring");
    let (r, w) = pipe2(O_CLOEXEC).expect("pipe2");

    let payload = b"hello uring";
    let written = write(w, payload).expect("write");
    assert_eq!(written, payload.len());

    let mut buf = [0u8; 32];
    // SAFETY: `buf` outlives the submission — submit_and_wait blocks
    // until the kernel finishes, then we reap before returning. The
    // buffer is on the stack and not moved during that interval.
    let sqe = unsafe {
        Sqe::read(
            r,
            buf.as_mut_ptr(),
            buf.len() as u32,
            u64::MAX, // -1 as off_t — stream, ignore offset
            0x42,
        )
    };
    ring.push_sqe(&sqe).expect("push_sqe");
    ring.submit_and_wait(1, 1).expect("submit_and_wait");

    let mut got: Option<Cqe> = None;
    let drained = ring.reap_cqes(|cqe| got = Some(cqe));
    assert_eq!(drained, 1);

    let cqe = got.expect("cqe");
    assert_eq!(cqe.user_data, 0x42);
    assert_eq!(cqe.res, payload.len() as i32);
    assert_eq!(&buf[..payload.len()], payload);

    let _ = close(r);
    let _ = close(w);
}

/// Once the SQ ring is full the next `push_sqe` returns
/// `IoErrorKind::WouldBlock` rather than corrupting the queue.
#[test]
fn push_until_full_returns_wouldblock() {
    // Request 4; kernel rounds up to 4 (already a power of two).
    let mut ring = Ring::new(4).expect("ring");
    for i in 0..4 {
        ring.push_sqe(&Sqe::nop(i)).expect("push within capacity");
    }
    let err = ring
        .push_sqe(&Sqe::nop(99))
        .expect_err("push should reject when full");
    assert_eq!(err.kind(), IoErrorKind::WouldBlock);
}

/// `Drop` releases all three mmaps and the ring fd without panicking
/// or returning an error path that propagates.
#[test]
fn drop_releases_without_panic() {
    let ring = Ring::new(8).expect("ring");
    drop(ring);
    // No assertion needed — reaching this line means Drop completed.
}

/// Round-trip a `WRITE` SQE: write 4 bytes to a pipe via io_uring,
/// then read them out the read end via a plain read syscall to
/// confirm the kernel actually wrote.
#[test]
fn write_round_trip() {
    let mut ring = Ring::new(8).expect("ring");
    let (r, w) = pipe2(O_CLOEXEC).expect("pipe2");

    let payload = b"abcd";
    // SAFETY: `payload` lives for the duration of submit_and_wait
    // + reap (a stack-allocated byte slice; we don't release it
    // until this fn returns, well after reap_cqes).
    let sqe = unsafe { Sqe::write(w, payload.as_ptr(), payload.len() as u32, u64::MAX, 0x7) };
    ring.push_sqe(&sqe).expect("push_sqe");
    ring.submit_and_wait(1, 1).expect("submit_and_wait");

    let mut got: Option<Cqe> = None;
    let drained = ring.reap_cqes(|cqe| got = Some(cqe));
    assert_eq!(drained, 1);
    let cqe = got.expect("cqe");
    assert_eq!(cqe.user_data, 0x7);
    assert_eq!(cqe.res, payload.len() as i32);

    // Verify the bytes actually landed by reading the read end.
    let mut buf = [0u8; 4];
    let n = crate::sys::linux::raw::read(r, &mut buf).expect("plain read");
    assert_eq!(n, 4);
    assert_eq!(&buf, payload);

    let _ = close(r);
    let _ = close(w);
}

/// `Cqe` should be `Copy + Clone + Debug`. Touch the Debug impl
/// so coverage credits it.
#[test]
fn cqe_debug_clone_copy() {
    let mut ring = Ring::new(4).expect("ring");
    ring.push_sqe(&Sqe::nop(123)).expect("push");
    ring.submit_and_wait(1, 1).expect("submit");
    let mut got: Option<Cqe> = None;
    ring.reap_cqes(|c| got = Some(c));
    let cqe = got.unwrap();
    let cqe2 = cqe; // Copy
    let cqe3 = Clone::clone(&cqe2);
    extern crate std;
    let s = std::format!("{cqe3:?}");
    assert!(s.contains("123"));
}

/// The `Ring::fd()` getter returns the underlying io_uring fd.
/// Coverage hits the trivial accessor; the value should be a valid
/// kernel-assigned fd (positive integer).
#[test]
fn ring_fd_getter_returns_positive() {
    let ring = Ring::new(8).expect("ring");
    let fd = ring.fd();
    assert!(fd > 0, "ring fd should be a positive kernel fd, got {fd}");
}

/// `submit` (without _and_wait) returns the number of consumed SQEs
/// without blocking the caller; the kernel processes them
/// asynchronously.
#[test]
fn submit_without_wait_consumes_pending() {
    let mut ring = Ring::new(8).expect("ring");
    ring.push_sqe(&Sqe::nop(1)).expect("push 1");
    ring.push_sqe(&Sqe::nop(2)).expect("push 2");
    let submitted = ring.submit(2).expect("submit");
    assert!(submitted >= 1);
    // The kernel will complete the NOPs asynchronously; wait for
    // at least one to land via a subsequent submit_and_wait.
    let _ = ring.submit_and_wait(0, 1);
    let mut count = 0;
    ring.reap_cqes(|_| count += 1);
    assert!(count >= 1, "at least one nop should have completed");
}
