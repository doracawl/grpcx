//! io_uring basic-functionality smoke test (T4.2 gate 2).
//!
//! Exercises every primitive added in T4.2: SQPOLL +
//! SQ_AFF set-up (with a fallback to the non-SQPOLL ring
//! when the kernel refuses), `IORING_REGISTER_FILES`,
//! fixed-file send, multishot recv (`IORING_OP_RECV` +
//! `IORING_RECV_MULTISHOT`), and the linked-send pair
//! (`IOSQE_IO_LINK`).
//!
//! Linux-only: the entire module is gated to
//! `cfg(target_os = "linux")` so non-Linux builds (macOS
//! / FreeBSD) compile to nothing. SQPOLL setup may fail
//! with `EPERM` on locked-down kernels; the SQPOLL test
//! treats that as a skip-not-fail.

#![cfg(all(feature = "iouring", target_os = "linux"))]

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use grpcx_io::IoErrorKind;
use grpcx_rt::iouring::{UringRing, drain_cqes, submit_linked_send_pair, submit_recv_multishot};

/// `Ring::new` should succeed on any io_uring-capable
/// kernel (5.1+). A NOP round-trip exercises the submit
/// / reap path end-to-end.
#[test]
fn ring_new_then_nop_roundtrip() {
    let mut ring = UringRing::new(8).expect("Ring::new");
    let nop = grpcx_io::Sqe::nop(0xCAFE);
    ring.push_sqe(&nop).expect("push_sqe");
    let _ = ring.submit_and_wait(1, 1).expect("submit_and_wait");

    let mut seen = None;
    drain_cqes(&mut ring, |c| {
        seen = Some(c);
    });
    let c = seen.expect("one CQE");
    assert_eq!(c.user_data, 0xCAFE);
    assert_eq!(c.res, 0, "NOP CQE.res should be 0");
}

/// SQPOLL set-up — if the kernel allows it, the
/// returned ring reports `has_sqpoll() == true` and a
/// NOP submit drives the kernel polling thread. If the
/// kernel refuses (`EPERM` / `BackendUnavailable`),
/// skip — the rest of T4.2 still works without SQPOLL.
#[test]
fn ring_new_sqpoll_then_nop() {
    match UringRing::new_sqpoll(8, /* cpu = */ 0, /* idle_ms = */ 100) {
        Ok(mut ring) => {
            assert!(ring.has_sqpoll());
            let nop = grpcx_io::Sqe::nop(0x5101_0011);
            ring.push_sqe(&nop).expect("push_sqe");
            // submit_and_wait is the same syscall path on
            // SQPOLL vs non-SQPOLL when min_complete > 0.
            let _ = ring.submit_and_wait(1, 1).expect("submit_and_wait");
            let mut seen = None;
            drain_cqes(&mut ring, |c| {
                seen = Some(c);
            });
            let c = seen.expect("one CQE");
            assert_eq!(c.user_data, 0x5101_0011);
            assert_eq!(c.res, 0);
        }
        Err(e)
            if e.kind() == IoErrorKind::BackendUnavailable
                || e.kind() == IoErrorKind::PermissionDenied =>
        {
            eprintln!("SQPOLL unavailable on this kernel — skipped ({e:?})");
        }
        Err(other) => panic!("unexpected SQPOLL failure: {other:?}"),
    }
}

/// Linked send pair on a `socketpair`: byte sequence
/// from the two sends must arrive in submission order on
/// the peer end, and both CQEs must carry the byte
/// count.
#[test]
fn linked_send_pair_over_socketpair() {
    let (a, b) = UnixStream::pair().expect("socketpair");
    let send_fd = a.as_raw_fd();
    let recv_fd = b.as_raw_fd();

    let mut ring = UringRing::new(8).expect("Ring::new");

    let first = b"AAAA";
    let second = b"BBBB";
    // SAFETY: both buffers outlive `submit_and_wait` /
    // `drain_cqes` and are not mutated.
    unsafe {
        submit_linked_send_pair(
            &mut ring,
            send_fd,
            first.as_ptr(),
            first.len() as u32,
            second.as_ptr(),
            second.len() as u32,
            0, // flags
            0x1111,
            0x2222,
        )
        .expect("submit_linked_send_pair");
    }
    let _ = ring.submit_and_wait(2, 2).expect("submit_and_wait");

    let mut got_first = false;
    let mut got_second = false;
    drain_cqes(&mut ring, |c| match c.user_data {
        0x1111 => {
            assert_eq!(c.res, first.len() as i32);
            got_first = true;
        }
        0x2222 => {
            assert_eq!(c.res, second.len() as i32);
            got_second = true;
        }
        other => panic!("unexpected user_data: {other:#x}"),
    });
    assert!(got_first && got_second, "both CQEs should drain");

    // Read back from the peer in submission order.
    use std::io::Read;
    let mut peer = b;
    let mut buf = [0u8; 8];
    peer.read_exact(&mut buf).expect("read peer");
    assert_eq!(&buf[..4], b"AAAA");
    assert_eq!(&buf[4..], b"BBBB");
    // Quiet the unused warning on the receiver fd binding.
    let _ = recv_fd;
}

/// Multishot recv: submit one `IORING_OP_RECV +
/// MULTISHOT` SQE, then write two distinct payloads on
/// the peer end. Expect two CQEs against the same
/// `user_data`. On kernels without multishot
/// (`-EINVAL`), skip cleanly.
#[test]
fn multishot_recv_two_payloads() {
    let (a, b) = UnixStream::pair().expect("socketpair");
    let recv_fd = b.as_raw_fd();
    let mut send_side = a;

    let mut ring = UringRing::new(16).expect("Ring::new");

    let mut buf = [0u8; 16];
    // SAFETY: `buf` lives across both multishot
    // completions (until `ring` is dropped at end of
    // function); no other writer is racing it.
    unsafe {
        submit_recv_multishot(
            &mut ring,
            recv_fd,
            buf.as_mut_ptr(),
            buf.len() as u32,
            0,
            0xABCD,
        )
        .expect("submit_recv_multishot");
    }
    let _ = ring.submit(1).expect("submit");

    use std::io::Write;
    send_side.write_all(b"hello").expect("write hello");

    let _ = ring.submit_and_wait(0, 1).expect("wait first");
    let mut completions = Vec::new();
    drain_cqes(&mut ring, |c| completions.push(c));
    assert!(
        !completions.is_empty(),
        "expected at least one multishot CQE after first write"
    );
    let first = &completions[0];
    if first.res == -22 {
        eprintln!("kernel does not support RECV_MULTISHOT (-EINVAL) — skipped");
        return;
    }
    assert_eq!(first.res, 5);

    send_side.write_all(b"world!").expect("write world");
    let _ = ring.submit_and_wait(0, 1).expect("wait second");
    let mut second_seen = false;
    drain_cqes(&mut ring, |c| {
        assert_eq!(c.user_data, 0xABCD);
        assert_eq!(c.res, 6);
        second_seen = true;
    });
    assert!(second_seen, "second multishot CQE should arrive");
}

/// Fixed-file table — register a socketpair fd, then
/// emit a send against the registered slot index
/// instead of the raw fd. The peer must see the
/// payload.
#[test]
fn fixed_file_send_via_registered_slot() {
    use grpcx_rt::iouring::{register_files, submit_send_fixed};

    let (a, b) = UnixStream::pair().expect("socketpair");
    let mut ring = UringRing::new(8).expect("Ring::new");

    let send_fd = a.as_raw_fd();
    let table = register_files(&ring, &[send_fd]).expect("register_files");
    assert_eq!(table.len(), 1);
    assert_eq!(table.fd_at(0), Some(send_fd));

    let payload = b"FIXED!";
    // SAFETY: `payload` is a static slice, lives forever.
    unsafe {
        submit_send_fixed(
            &mut ring,
            /* slot = */ 0,
            payload.as_ptr(),
            payload.len() as u32,
            0,
            0xF1F1,
        )
        .expect("submit_send_fixed");
    }
    let _ = ring.submit_and_wait(1, 1).expect("submit_and_wait");

    let mut seen = None;
    drain_cqes(&mut ring, |c| {
        seen = Some(c);
    });
    let c = seen.expect("one CQE");
    assert_eq!(c.user_data, 0xF1F1);
    assert_eq!(c.res, payload.len() as i32);

    use std::io::Read;
    let mut peer = b;
    let mut got = [0u8; 6];
    peer.read_exact(&mut got).expect("read peer");
    assert_eq!(&got, payload);

    let _ = table; // record is a plain value; no Drop work
}
