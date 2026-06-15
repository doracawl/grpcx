//! `io_uring` ring fd + three mmap regions.
//!
//! Per `io_uring_setup(2)`, the kernel exposes three byte regions
//! addressable via `mmap` at distinct offset constants:
//!
//! | Region    | mmap offset                  | Contains                                      |
//! |-----------|------------------------------|-----------------------------------------------|
//! | SQ ring   | `IORING_OFF_SQ_RING`         | head / tail / dropped + the SQE index array   |
//! | CQ ring   | `IORING_OFF_CQ_RING`         | head / tail / overflow + the CQE array        |
//! | SQEs     | `IORING_OFF_SQES`            | the backing array of SQE entries              |
//!
//! Kernels >= 5.4 set `IORING_FEAT_SINGLE_MMAP`, which lets the SQ
//! and CQ ring share one mapping; this module collapses the two
//! mappings into one when the feature is advertised.
//!
//! T18 stops at the bare regions + head/tail accessors. T19 (SQE
//! encode) and T20 (CQE reap) build on top.

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use super::{Cqe, Sqe};
use crate::sys::linux::raw::{
    self, IORING_ENTER_GETEVENTS, IORING_FEAT_SINGLE_MMAP, IORING_OFF_CQ_RING, IORING_OFF_SQ_RING,
    IORING_OFF_SQES, IoUringCqe, IoUringParams, MAP_POPULATE, MAP_SHARED, PROT_READ, PROT_WRITE,
    RawFd,
};
use crate::types::{IoError, IoErrorKind, IoResult};

/// Pretend size of an io_uring SQE in bytes. The default ring layout
/// uses 64-byte SQEs; the `IORING_SETUP_SQE128` flag (kernels >= 5.19)
/// doubles this and is not used at L1.
const SQE_SIZE: usize = 64;

/// Pretend size of an io_uring CQE in bytes. Default layout; the
/// `IORING_SETUP_CQE32` flag (kernels >= 5.19) doubles this.
const CQE_SIZE: usize = 16;

/// Errno values that mean "the kernel does not (or refuses to)
/// expose io_uring on this system" and should fall back to epoll.
const ENOSYS: i32 = 38;
const EINVAL: i32 = 22;
const EPERM: i32 = 1;

/// Owning handle for a Linux `io_uring` instance.
///
/// Wraps the ring fd plus the SQ-ring / CQ-ring / SQE-array mmap
/// regions, and exposes the four core queue operations:
///
/// - [`push_sqe`] — reserve a slot, copy in the [`Sqe`], publish via
///   the SQ tail (single-producer).
/// - [`submit`] — notify the kernel of pending SQEs and return.
/// - [`submit_and_wait`] — submit then block until N completions are
///   visible.
/// - [`reap_cqes`] — drain available [`Cqe`]s via a callback
///   (single-consumer).
///
/// `Ring` is `Send` but not `Sync`. Concurrent multi-producer or
/// multi-consumer use requires explicit external synchronization —
/// the SPSC discipline is the cost-minimal default.
///
/// `Ring` is the **completion-based** counterpart to the
/// readiness-based [`Poll`] reactor and ships as a sibling, not a
/// substitute. Forcing completion semantics under a readiness API
/// would obscure io_uring's cost model (the caller's choice between
/// "submit one op and wait" vs "drain N ops in one syscall" only
/// shows up when the API surfaces it). The full reasoning lives in
/// the v0.4 refactor doc at the workspace root.
///
/// [`push_sqe`]: Self::push_sqe
/// [`submit`]: Self::submit
/// [`submit_and_wait`]: Self::submit_and_wait
/// [`reap_cqes`]: Self::reap_cqes
/// [`Poll`]: crate::Poll
pub struct Ring {
    fd: RawFd,
    sq_ring_ptr: *mut u8,
    sq_ring_size: usize,
    cq_ring_ptr: *mut u8,
    cq_ring_size: usize,
    sqes_ptr: *mut u8,
    sqes_size: usize,
    params: IoUringParams,
}

// SAFETY: ownership of the ring fd + three mmap regions is exclusive
// to the `Ring` value; no shared mutability is reachable through it
// today. Sending the value across threads transfers all of that
// exclusively. `!Sync` is conveyed by the raw-pointer fields (and is
// correct — concurrent submission requires explicit SPSC discipline
// that T19/T20 will introduce on top).
unsafe impl Send for Ring {}

impl Ring {
    /// Create a new ring with at least `entries` SQ slots.
    ///
    /// `entries` is rounded up to a power of two by the kernel
    /// (minimum 1, maximum 4096 by default — see `io_uring_setup(2)`).
    ///
    /// Translates `ENOSYS` / `EINVAL` / `EPERM` from `io_uring_setup`
    /// to [`IoErrorKind::BackendUnavailable`] so callers on kernels
    /// older than 5.1, or in sandboxed environments that disable
    /// io_uring at the seccomp layer, can transparently fall back to
    /// the epoll backend without distinguishing the failure mode.
    pub fn new(entries: u32) -> IoResult<Self> {
        let mut params = IoUringParams::default();
        let fd = match raw::io_uring_setup(entries, &mut params) {
            Ok(fd) => fd,
            Err(e) => {
                return Err(match e.raw_os_code() {
                    Some(ENOSYS) | Some(EINVAL) | Some(EPERM) => IoError::new(
                        IoErrorKind::BackendUnavailable,
                        e.raw_os_code().unwrap_or(0),
                    ),
                    _ => e,
                });
            }
        };

        // Region-size math from `io_uring_setup(2)`:
        //   sq_ring_size = sq_off.array + sq_entries * sizeof(u32)
        //   cq_ring_size = cq_off.cqes  + cq_entries * sizeof(cqe)
        //   sqes_size    = sq_entries   * sizeof(sqe)
        let mut sq_ring_size =
            params.sq_off.array as usize + params.sq_entries as usize * size_of::<u32>();
        let mut cq_ring_size = params.cq_off.cqes as usize + params.cq_entries as usize * CQE_SIZE;
        let sqes_size = params.sq_entries as usize * SQE_SIZE;

        // On kernels >= 5.4 the SQ and CQ ring share one mapping; the
        // single mmap covers max(sq_ring_size, cq_ring_size).
        let single_mmap = (params.features & IORING_FEAT_SINGLE_MMAP) != 0;
        if single_mmap {
            let combined = sq_ring_size.max(cq_ring_size);
            sq_ring_size = combined;
            cq_ring_size = combined;
        }

        let sq_ring_ptr = match raw::mmap(
            0,
            sq_ring_size,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_POPULATE,
            fd,
            IORING_OFF_SQ_RING,
        ) {
            Ok(p) => p,
            Err(e) => {
                let _ = raw::close(fd);
                return Err(e);
            }
        };

        let cq_ring_ptr = if single_mmap {
            sq_ring_ptr
        } else {
            match raw::mmap(
                0,
                cq_ring_size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED | MAP_POPULATE,
                fd,
                IORING_OFF_CQ_RING,
            ) {
                Ok(p) => p,
                Err(e) => {
                    let _ = raw::munmap(sq_ring_ptr, sq_ring_size);
                    let _ = raw::close(fd);
                    return Err(e);
                }
            }
        };

        let sqes_ptr = match raw::mmap(
            0,
            sqes_size,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_POPULATE,
            fd,
            IORING_OFF_SQES,
        ) {
            Ok(p) => p,
            Err(e) => {
                if !single_mmap {
                    let _ = raw::munmap(cq_ring_ptr, cq_ring_size);
                }
                let _ = raw::munmap(sq_ring_ptr, sq_ring_size);
                let _ = raw::close(fd);
                return Err(e);
            }
        };

        Ok(Self {
            fd,
            sq_ring_ptr,
            sq_ring_size,
            cq_ring_ptr,
            cq_ring_size,
            sqes_ptr,
            sqes_size,
            params,
        })
    }

    /// The ring fd.
    #[inline]
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Bitmask for SQ indices. The kernel rounds `sq_entries` up to
    /// a power of two, so `entries - 1` is the wrap-around mask.
    #[inline]
    fn sq_ring_mask(&self) -> u32 {
        self.params.sq_entries - 1
    }

    /// Bitmask for CQ indices.
    #[inline]
    fn cq_ring_mask(&self) -> u32 {
        self.params.cq_entries - 1
    }

    /// Pointer to the SQ index array (maps SQ position → SQE index).
    #[inline]
    fn sq_array_ptr(&self) -> *mut u32 {
        // SAFETY: `sq_off.array` is a u32-aligned byte offset inside
        // the SQ-ring mapping; the returned pointer stays valid for
        // the lifetime of `self`.
        unsafe { self.sq_ring_ptr.add(self.params.sq_off.array as usize) as *mut u32 }
    }

    /// Pointer to the CQE array inside the CQ-ring mapping.
    #[inline]
    fn cqes_ptr(&self) -> *const IoUringCqe {
        // SAFETY: `cq_off.cqes` is a 16-byte-aligned byte offset
        // inside the CQ-ring mapping.
        unsafe { self.cq_ring_ptr.add(self.params.cq_off.cqes as usize) as *const IoUringCqe }
    }

    /// Reserve a slot, copy `sqe` into it, register it in the SQ
    /// index array, and publish by advancing the SQ tail.
    ///
    /// Returns [`IoErrorKind::WouldBlock`] if the queue has no free
    /// slot (kernel hasn't reaped enough SQEs yet).
    ///
    /// Single-producer: only one thread / call site may push at a
    /// time. The `&mut self` receiver enforces that statically.
    pub fn push_sqe(&mut self, sqe: &Sqe) -> IoResult<()> {
        let tail = self.sq_tail().load(Ordering::Acquire);
        let head = self.sq_head().load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= self.params.sq_entries {
            return Err(IoError::new(IoErrorKind::WouldBlock, 11));
        }

        let slot = (tail & self.sq_ring_mask()) as usize;

        // SAFETY: `sqes_ptr` points to an mmap region of length
        // `sq_entries * 64` bytes; `slot < sq_entries`; the SQE slot
        // is not concurrently read by the kernel until we publish
        // via the Release store on `sq_tail` below.
        unsafe {
            let dst = self.sqes_ptr as *mut crate::sys::linux::raw::IoUringSqe;
            core::ptr::write(dst.add(slot), *sqe.as_raw());
        }

        // SAFETY: `sq_array_ptr` points to `sq_entries` u32 slots
        // inside the SQ-ring mapping; same publication-via-tail
        // story as above.
        unsafe {
            let array = self.sq_array_ptr();
            core::ptr::write(array.add(slot), slot as u32);
        }

        self.sq_tail()
            .store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Notify the kernel of `to_submit` newly published SQEs and
    /// return immediately. Returns the number actually consumed.
    pub fn submit(&self, to_submit: u32) -> IoResult<i32> {
        raw::io_uring_enter(self.fd, to_submit, 0, 0)
    }

    /// Submit `to_submit` SQEs and block in the kernel until at least
    /// `min_complete` CQEs are available.
    pub fn submit_and_wait(&self, to_submit: u32, min_complete: u32) -> IoResult<i32> {
        raw::io_uring_enter(self.fd, to_submit, min_complete, IORING_ENTER_GETEVENTS)
    }

    /// Drain every available CQE, invoking `f` on each. Returns the
    /// number drained.
    ///
    /// Single-consumer: only one thread / call site may reap at a
    /// time. The `&mut self` receiver enforces that statically.
    pub fn reap_cqes<F: FnMut(Cqe)>(&mut self, mut f: F) -> u32 {
        let mask = self.cq_ring_mask();
        let head_start = self.cq_head().load(Ordering::Relaxed);
        let tail = self.cq_tail().load(Ordering::Acquire);

        let mut count: u32 = 0;
        let mut cur = head_start;
        while cur != tail {
            let slot = (cur & mask) as usize;
            // SAFETY: `cqes_ptr` points to `cq_entries * 16` bytes
            // inside the CQ-ring mapping; `slot < cq_entries`. The
            // CQE was published by the kernel before incrementing
            // `cq_tail` (which we observed with Acquire), so the
            // bytes are valid.
            let raw_cqe: IoUringCqe = unsafe { *self.cqes_ptr().add(slot) };
            f(Cqe::from_raw(raw_cqe));
            cur = cur.wrapping_add(1);
            count += 1;
        }

        if count > 0 {
            self.cq_head().store(tail, Ordering::Release);
        }
        count
    }

    /// SQ ring `head` counter. The kernel reads this with `Acquire`
    /// to know how many SQEs userspace has already consumed; T19
    /// readers use `Acquire` loads to observe kernel progress.
    #[inline]
    pub(crate) fn sq_head(&self) -> &AtomicU32 {
        // SAFETY: `sq_off.head` is a u32-aligned byte offset into the
        // SQ-ring mapping, set by the kernel at `io_uring_setup`. The
        // returned reference is bound to `&self`, so the mapping
        // outlives every borrow (Drop only runs after all borrows
        // have ended).
        unsafe { atomic_at(self.sq_ring_ptr, self.params.sq_off.head) }
    }

    /// SQ ring `tail` counter. Userspace publishes new SQE indices by
    /// incrementing `tail` with `Release` ordering after writing the
    /// index array entry.
    #[inline]
    pub(crate) fn sq_tail(&self) -> &AtomicU32 {
        // SAFETY: see `sq_head` — `sq_off.tail` is u32-aligned.
        unsafe { atomic_at(self.sq_ring_ptr, self.params.sq_off.tail) }
    }

    /// CQ ring `head` counter. Userspace advances this with `Release`
    /// to reclaim consumed CQE slots after reaping.
    #[inline]
    pub(crate) fn cq_head(&self) -> &AtomicU32 {
        // SAFETY: see `sq_head` — applied to the CQ mapping.
        unsafe { atomic_at(self.cq_ring_ptr, self.params.cq_off.head) }
    }

    /// CQ ring `tail` counter. Userspace observes this with `Acquire`
    /// to discover new completions published by the kernel.
    #[inline]
    pub(crate) fn cq_tail(&self) -> &AtomicU32 {
        // SAFETY: see `sq_head` — applied to the CQ mapping.
        unsafe { atomic_at(self.cq_ring_ptr, self.params.cq_off.tail) }
    }
}

/// Resolve `base + offset` as a `&AtomicU32` into a kernel ring
/// region.
///
/// # Safety
///
/// - `base` must point to a live mmap region of at least
///   `offset + 4` bytes.
/// - `offset` must be a 4-byte-aligned offset inside that region.
/// - The returned reference's lifetime must not outlive the region.
#[inline]
unsafe fn atomic_at<'a>(base: *mut u8, offset: u32) -> &'a AtomicU32 {
    let p = base.wrapping_add(offset as usize) as *const AtomicU32;
    // SAFETY: contract delegated to the caller.
    unsafe { &*p }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // Best-effort cleanup: unmap each region, then close the fd.
        // Errors are ignored — there is nowhere to surface them from
        // a destructor, and the kernel reclaims everything when the
        // process exits regardless.
        let single_mmap = (self.params.features & IORING_FEAT_SINGLE_MMAP) != 0;
        let _ = raw::munmap(self.sqes_ptr, self.sqes_size);
        if !single_mmap {
            let _ = raw::munmap(self.cq_ring_ptr, self.cq_ring_size);
        }
        let _ = raw::munmap(self.sq_ring_ptr, self.sq_ring_size);
        let _ = raw::close(self.fd);
    }
}
