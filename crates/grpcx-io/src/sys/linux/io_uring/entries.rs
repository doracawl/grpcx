//! Public submission / completion entry types for the io_uring
//! backend.
//!
//! These wrap the kernel-ABI structs in `sys::linux::raw` and present
//! a stable, opaque surface to callers. The wire layout can change
//! without breaking the public API.

use crate::sys::linux::raw::{
    IORING_OP_NOP, IORING_OP_READ, IORING_OP_WRITE, IoUringCqe, IoUringSqe, RawFd,
};

/// io_uring submission-queue entry — a fully-formed work item ready
/// to push to a [`Ring`].
///
/// Construct one with [`Sqe::nop`], [`Sqe::read`], or [`Sqe::write`],
/// then hand it to [`Ring::push_sqe`].
///
/// The inner layout matches `linux/io_uring.h`'s `struct
/// io_uring_sqe`; the wrapper hides the kernel field names so the
/// public API can evolve without breaking the wire format.
///
/// [`Ring`]: super::Ring
/// [`Ring::push_sqe`]: super::Ring::push_sqe
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Sqe(IoUringSqe);

impl Sqe {
    /// Build a `NOP` SQE. The kernel completes it immediately with
    /// `res = 0`; the `user_data` echoes back as
    /// [`Cqe::user_data`]. Useful for smoke-testing the ring
    /// end-to-end without touching any real fd.
    #[inline]
    pub fn nop(user_data: u64) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_NOP,
            user_data,
            ..IoUringSqe::default()
        })
    }

    /// Build a `READ` SQE.
    ///
    /// The kernel reads up to `len` bytes from `fd` into the buffer
    /// `buf` points at. `off` is the file offset (use `u64::MAX` —
    /// i.e. `-1` as `off_t` — for streams / sockets where the
    /// offset is meaningless). `user_data` echoes back in the CQE.
    ///
    /// # Safety
    ///
    /// `buf` must remain valid for `len` bytes of writes from the
    /// time [`Ring::push_sqe`] returns until the matching
    /// [`Cqe`] has been observed via [`Ring::reap_cqes`]. The kernel
    /// writes to this region asynchronously; freeing or reusing it
    /// before completion is a use-after-free.
    ///
    /// [`Ring::push_sqe`]: super::Ring::push_sqe
    /// [`Ring::reap_cqes`]: super::Ring::reap_cqes
    #[inline]
    pub unsafe fn read(fd: RawFd, buf: *mut u8, len: u32, off: u64, user_data: u64) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_READ,
            fd,
            addr: buf as u64,
            len,
            off,
            user_data,
            ..IoUringSqe::default()
        })
    }

    /// Build a `WRITE` SQE.
    ///
    /// The kernel reads up to `len` bytes from `buf` and writes them
    /// to `fd`. `off` is the file offset; pass `u64::MAX` for
    /// streams / sockets. `user_data` echoes back in the CQE.
    ///
    /// # Safety
    ///
    /// `buf` must remain valid for `len` bytes of reads for the
    /// same duration as in [`Sqe::read`]. See its safety note.
    #[inline]
    pub unsafe fn write(fd: RawFd, buf: *const u8, len: u32, off: u64, user_data: u64) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_WRITE,
            fd,
            addr: buf as u64,
            len,
            off,
            user_data,
            ..IoUringSqe::default()
        })
    }

    /// Borrow the inner kernel struct. `pub(crate)`-only; the public
    /// API hides the layout.
    #[inline]
    pub(crate) fn as_raw(&self) -> &IoUringSqe {
        &self.0
    }
}

/// io_uring completion-queue entry — one completed operation.
///
/// Produced by [`Ring::reap_cqes`]. `user_data` echoes back from the
/// originating [`Sqe`]; route on it to pair completions with
/// submissions.
///
/// [`Ring::reap_cqes`]: super::Ring::reap_cqes
#[derive(Clone, Copy, Debug)]
pub struct Cqe {
    /// User-supplied cookie copied from the originating SQE's
    /// `user_data` field.
    pub user_data: u64,
    /// Kernel return value: non-negative on success (typically a
    /// byte count or handle), negative `-errno` on failure.
    pub res: i32,
    /// CQE flag bits — see `IORING_CQE_F_*` in `linux/io_uring.h`.
    pub flags: u32,
}

impl Cqe {
    /// Convert from the kernel struct. `pub(crate)`-only.
    #[inline]
    pub(crate) fn from_raw(raw: IoUringCqe) -> Self {
        Self {
            user_data: raw.user_data,
            res: raw.res,
            flags: raw.flags,
        }
    }
}
