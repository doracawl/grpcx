//! Fixed-file and fixed-buffer registration helpers.
//!
//! Two layers:
//!
//! 1. **Side-effecting registration functions** —
//!    [`register_files`], [`unregister_files`],
//!    [`register_buffers`], [`unregister_buffers`] —
//!    each just calls the matching `io_uring_register`
//!    opcode through the borrowed ring.
//! 2. **Value-typed lookup records** — [`FixedFileTable`]
//!    and [`RegisteredBuffers`] — record what was
//!    registered so call sites can ask "what fd is at
//!    slot N?" without re-querying the kernel.
//!
//! The records have **no `Drop` and no borrow on the
//! ring**, so they don't conflict with later
//! `&mut UringRing` submits. Closing the ring (via
//! `Drop` on [`UringRing`]) implicitly releases any
//! registrations the kernel still holds for it — so
//! explicit unregister is opt-in (call it to free a
//! table before the ring goes away).

use alloc::vec::Vec;
use grpcx_io::RawFd;

use super::ring::UringRing;

/// Value-typed record of a registered file-descriptor
/// table. Hold one alongside the ring to translate slot
/// indices back to raw fds without a kernel round-trip.
#[derive(Debug, Clone, Default)]
pub struct FixedFileTable {
    slots: Vec<RawFd>,
}

impl FixedFileTable {
    /// Number of registered slots.
    #[inline]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the table is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Lookup the raw fd at `slot`.
    #[inline]
    pub fn fd_at(&self, slot: usize) -> Option<RawFd> {
        self.slots.get(slot).copied()
    }
}

/// Value-typed record of a registered buffer table.
#[derive(Debug, Clone, Default)]
pub struct RegisteredBuffers {
    count: usize,
}

impl RegisteredBuffers {
    /// Number of registered buffer slots.
    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the table is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Register `fds` with `ring` as a fixed-file table
/// (`IORING_REGISTER_FILES`). Returns a value-typed
/// record so call sites can later resolve slot indices.
///
/// Subsequent SQEs flagged with `IOSQE_FIXED_FILE` (see
/// [`super::ops::submit_send_fixed`]) treat their `fd`
/// field as an index into this table.
pub fn register_files(ring: &UringRing, fds: &[RawFd]) -> grpcx_io::IoResult<FixedFileTable> {
    ring.inner().register_files(fds)?;
    Ok(FixedFileTable {
        slots: fds.to_vec(),
    })
}

/// Drop the registered file table. Optional: closing the
/// ring releases it implicitly, but call this to free the
/// kernel-side allocation before then.
pub fn unregister_files(ring: &UringRing) -> grpcx_io::IoResult<()> {
    ring.inner().unregister_files()
}

/// Register `bufs` with `ring` as a fixed-buffer table
/// (`IORING_REGISTER_BUFFERS`). Each `(ptr, len)` pair
/// becomes one indexed iovec.
///
/// # Safety
///
/// Every `(ptr, len)` slice must satisfy the kernel's
/// pinning contract — the region must stay valid for the
/// lifetime of the registration (until matching
/// [`unregister_buffers`] or ring drop). Move, free, or
/// reuse before then is a use-after-free seen from the
/// kernel.
pub unsafe fn register_buffers(
    ring: &UringRing,
    bufs: &[(*mut u8, usize)],
) -> grpcx_io::IoResult<RegisteredBuffers> {
    // SAFETY: contract delegated to the caller.
    unsafe {
        ring.inner().register_buffers(bufs)?;
    }
    Ok(RegisteredBuffers { count: bufs.len() })
}

/// Drop the registered buffer table.
pub fn unregister_buffers(ring: &UringRing) -> grpcx_io::IoResult<()> {
    ring.inner().unregister_buffers()
}
