//! Typed SQE constructors / submitters for the v2
//! transport's hot path.
//!
//! Each function builds the right `grpcx_io::Sqe`, pushes
//! it onto the ring, and returns immediately — flush is
//! the caller's responsibility via [`super::ring::UringRing::submit`]
//! so a batched submit can amortise the cost across many
//! consecutive `submit_*` calls.
//!
//! `user_data` is the opaque cookie the kernel echoes back
//! in the matching CQE; the v2 transport packs
//! `(stream_id << 8) | op_tag` into it so the completion
//! drain can route without a lookup.

use grpcx_io::{RawFd, Sqe};

use super::ring::UringRing;

/// Submit a `SEND` SQE for stream-oriented `fd`. `flags`
/// mirrors `send(2)` flags (e.g. `MSG_NOSIGNAL = 0x4000`
/// on Linux).
///
/// # Safety
///
/// `buf` must remain valid for `len` bytes of reads from
/// the moment of submission until the matching CQE has
/// been observed via [`super::completion::drain_cqes`].
pub unsafe fn submit_send(
    ring: &mut UringRing,
    fd: RawFd,
    buf: *const u8,
    len: u32,
    flags: u32,
    user_data: u64,
) -> grpcx_io::IoResult<()> {
    // SAFETY: caller upholds the buffer-lifetime contract;
    // `Sqe::send` itself is `unsafe` for the same reason
    // and the requirement propagates.
    let sqe = unsafe { Sqe::send(fd, buf, len, flags, user_data) };
    ring.push_sqe(&sqe)
}

/// Submit a `SEND` SQE addressed to a slot in the
/// registered fixed-file table (set up via
/// [`super::register::FixedFileTable`]). `slot` replaces
/// the `fd` field; the kernel resolves it through the
/// table without a per-syscall lookup.
///
/// # Safety
///
/// Same buffer-lifetime contract as [`submit_send`].
pub unsafe fn submit_send_fixed(
    ring: &mut UringRing,
    slot: u32,
    buf: *const u8,
    len: u32,
    flags: u32,
    user_data: u64,
) -> grpcx_io::IoResult<()> {
    // SAFETY: see [`submit_send`].
    let sqe = unsafe { Sqe::send(slot as RawFd, buf, len, flags, user_data).with_fixed_file() };
    ring.push_sqe(&sqe)
}

/// Submit a `RECV` SQE for stream-oriented `fd`.
///
/// # Safety
///
/// `buf` must remain valid for `len` bytes of writes from
/// submission until the matching CQE has been observed.
pub unsafe fn submit_recv(
    ring: &mut UringRing,
    fd: RawFd,
    buf: *mut u8,
    len: u32,
    flags: u32,
    user_data: u64,
) -> grpcx_io::IoResult<()> {
    // SAFETY: see the doc comment.
    let sqe = unsafe { Sqe::recv(fd, buf, len, flags, user_data) };
    ring.push_sqe(&sqe)
}

/// Submit a multishot `RECV` SQE — the kernel reposts a
/// CQE every time bytes arrive on `fd` until the socket
/// closes or errors, without the caller resubmitting.
///
/// Requires kernel >= 6.0 (the `IORING_RECV_MULTISHOT`
/// flag). On older kernels the completion arrives with
/// `res = -EINVAL` and the caller should fall back to
/// per-arrival [`submit_recv`].
///
/// # Safety
///
/// `buf` must remain valid for `len` bytes of writes for
/// the **entire** multishot lifecycle (not just the first
/// CQE). Use a dedicated per-connection buffer.
pub unsafe fn submit_recv_multishot(
    ring: &mut UringRing,
    fd: RawFd,
    buf: *mut u8,
    len: u32,
    flags: u32,
    user_data: u64,
) -> grpcx_io::IoResult<()> {
    // SAFETY: see the doc comment.
    let sqe = unsafe { Sqe::recv_multishot(fd, buf, len, flags, user_data) };
    ring.push_sqe(&sqe)
}

/// Submit two linked SEND SQEs as one atomic chain — the
/// second only starts if the first completed successfully
/// (`res >= 0`); a failure cascades as `-ECANCELED` on the
/// second's CQE.
///
/// Designed for the v2 framing pattern of header + payload
/// on one stream id: the chain guarantees the payload
/// never lands without its preceding header on the wire
/// even if the kernel reorders unlinked SQEs internally.
///
/// `user_data_first` / `user_data_second` distinguish the
/// two completions in the drain.
///
/// # Safety
///
/// Both buffer pointers must remain valid for their
/// respective lengths until **both** CQEs have been
/// observed (the second carries the success / failure
/// verdict on the chain).
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_linked_send_pair(
    ring: &mut UringRing,
    fd: RawFd,
    first_buf: *const u8,
    first_len: u32,
    second_buf: *const u8,
    second_len: u32,
    flags: u32,
    user_data_first: u64,
    user_data_second: u64,
) -> grpcx_io::IoResult<()> {
    // SAFETY: caller upholds both buffers' lifetimes.
    let first = unsafe { Sqe::send(fd, first_buf, first_len, flags, user_data_first).linked() };
    let second = unsafe { Sqe::send(fd, second_buf, second_len, flags, user_data_second) };
    // Push both before submit; the kernel sees them as a
    // chain because of the IO_LINK flag on the first.
    ring.push_sqe(&first)?;
    ring.push_sqe(&second)
}
