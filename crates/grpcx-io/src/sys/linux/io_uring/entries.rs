//! Public submission / completion entry types for the io_uring
//! backend.
//!
//! These wrap the kernel-ABI structs in `sys::linux::raw` and present
//! a stable, opaque surface to callers. The wire layout can change
//! without breaking the public API.

use crate::sys::linux::raw::{
    IORING_OP_NOP, IORING_OP_READ, IORING_OP_RECV, IORING_OP_SEND, IORING_OP_WRITE,
    IORING_RECV_MULTISHOT, IOSQE_FIXED_FILE, IOSQE_IO_LINK, IoUringCqe, IoUringSqe, RawFd,
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

    /// Build a `SEND` SQE.
    ///
    /// The kernel sends up to `len` bytes from `buf` over the
    /// stream-oriented socket `fd`. `flags` mirrors `send(2)` flags
    /// (`MSG_NOSIGNAL` is the common choice; pass `0` for the default).
    /// `user_data` echoes back in the CQE.
    ///
    /// # Safety
    ///
    /// `buf` must remain valid for `len` bytes of reads from the time
    /// [`Ring::push_sqe`] returns until the matching [`Cqe`] has been
    /// observed via [`Ring::reap_cqes`].
    ///
    /// [`Ring::push_sqe`]: super::Ring::push_sqe
    /// [`Ring::reap_cqes`]: super::Ring::reap_cqes
    #[inline]
    pub unsafe fn send(fd: RawFd, buf: *const u8, len: u32, flags: u32, user_data: u64) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_SEND,
            fd,
            addr: buf as u64,
            len,
            op_flags: flags,
            user_data,
            ..IoUringSqe::default()
        })
    }

    /// Build a `RECV` SQE.
    ///
    /// The kernel reads up to `len` bytes from the stream-oriented
    /// socket `fd` into `buf`. `flags` mirrors `recv(2)` flags
    /// (`0` for default).
    ///
    /// # Safety
    ///
    /// `buf` must remain valid for `len` bytes of writes for the same
    /// duration as in [`Sqe::send`].
    #[inline]
    pub unsafe fn recv(fd: RawFd, buf: *mut u8, len: u32, flags: u32, user_data: u64) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_RECV,
            fd,
            addr: buf as u64,
            len,
            op_flags: flags,
            user_data,
            ..IoUringSqe::default()
        })
    }

    /// Build a multishot `RECV` SQE.
    ///
    /// The kernel reposts a CQE every time bytes arrive on `fd` until
    /// it errors or is explicitly cancelled, without the caller having
    /// to resubmit. Requires kernel >= 6.0; on older kernels the
    /// completion arrives with `res = -EINVAL`.
    ///
    /// Without a registered buffer pool (`BUF_RING`), each completion
    /// writes into the single caller-provided buffer — useful for
    /// smoke tests; production callers should pair this with the
    /// buffer-select path once it lands.
    ///
    /// # Safety
    ///
    /// Same as [`Sqe::recv`], but the buffer must stay live across the
    /// **entire** multishot lifecycle, not just the first CQE.
    #[inline]
    pub unsafe fn recv_multishot(
        fd: RawFd,
        buf: *mut u8,
        len: u32,
        flags: u32,
        user_data: u64,
    ) -> Self {
        Self(IoUringSqe {
            opcode: IORING_OP_RECV,
            fd,
            addr: buf as u64,
            len,
            op_flags: flags,
            user_data,
            ioprio: IORING_RECV_MULTISHOT,
            ..IoUringSqe::default()
        })
    }

    /// Set the `IOSQE_IO_LINK` flag — the next SQE pushed onto the
    /// ring will not start until this one completes successfully (a
    /// non-zero `res` short-circuits the chain with `-ECANCELED` on
    /// the linked entry).
    #[inline]
    #[must_use]
    pub fn linked(mut self) -> Self {
        self.0.flags |= IOSQE_IO_LINK;
        self
    }

    /// Set the `IOSQE_FIXED_FILE` flag — the `fd` field is interpreted
    /// as a slot index into the registered file table
    /// ([`Ring::register_files`]) rather than a raw kernel fd.
    ///
    /// [`Ring::register_files`]: super::Ring::register_files
    #[inline]
    #[must_use]
    pub fn with_fixed_file(mut self) -> Self {
        self.0.flags |= IOSQE_FIXED_FILE;
        self
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
