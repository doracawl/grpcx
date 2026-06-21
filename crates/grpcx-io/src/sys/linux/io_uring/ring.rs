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
    self, IORING_ENTER_GETEVENTS, IORING_ENTER_SQ_WAKEUP, IORING_FEAT_SINGLE_MMAP,
    IORING_OFF_CQ_RING, IORING_OFF_SQ_RING, IORING_OFF_SQES, IORING_REGISTER_BUFFERS,
    IORING_REGISTER_FILES, IORING_SETUP_SQ_AFF, IORING_SETUP_SQPOLL, IORING_SQ_NEED_WAKEUP,
    IORING_UNREGISTER_BUFFERS, IORING_UNREGISTER_FILES, IoUringCqe, IoUringParams, IoVec,
    MAP_POPULATE, MAP_SHARED, PROT_READ, PROT_WRITE, RawFd,
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
        Self::new_with_params(entries, IoUringParams::default())
    }

    /// Create a ring backed by a pinned SQPOLL kernel thread.
    ///
    /// Sets `IORING_SETUP_SQPOLL | IORING_SETUP_SQ_AFF`, pins the
    /// polling thread to logical CPU `cpu`, and lets it sleep after
    /// `idle_ms` of inactivity (the kernel then needs an
    /// `IORING_ENTER_SQ_WAKEUP` to resume). When SQPOLL is active,
    /// userspace can publish SQEs and skip the syscall entirely on
    /// the fast path — see [`Self::submit_sqpoll`].
    ///
    /// SQPOLL on Linux kernels before 5.11 needs `CAP_SYS_NICE`; on
    /// kernels 5.13 and later, unprivileged callers can use it when
    /// the kernel was built with
    /// `CONFIG_IO_URING_SQPOLL_UNPRIVILEGED=y`. When the kernel
    /// refuses, this returns [`IoErrorKind::BackendUnavailable`] so
    /// the caller can fall back to [`Self::new`] (no SQPOLL).
    pub fn new_sqpoll(entries: u32, cpu: u32, idle_ms: u32) -> IoResult<Self> {
        let mut params = IoUringParams {
            flags: IORING_SETUP_SQPOLL | IORING_SETUP_SQ_AFF,
            sq_thread_cpu: cpu,
            sq_thread_idle: idle_ms,
            ..IoUringParams::default()
        };
        // Older kernels reject the SQ_AFF + cpu pair when the cpu is
        // offline / out of range — surface that as
        // `BackendUnavailable` too, so the caller has a single
        // fallback branch.
        Self::new_with_params(entries, core::mem::take(&mut params))
    }

    fn new_with_params(entries: u32, mut params: IoUringParams) -> IoResult<Self> {
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

    /// Fast-path submit for SQPOLL rings: skip the syscall when the
    /// kernel polling thread is awake. Returns the number of SQEs
    /// the kernel consumed (0 if the thread was awake and picked
    /// them up without needing a wakeup).
    ///
    /// On a non-SQPOLL ring this is identical to [`Self::submit`]
    /// since the `IORING_SQ_NEED_WAKEUP` flag is never set by the
    /// kernel; callers should still prefer the named API for
    /// readability.
    pub fn submit_sqpoll(&self, to_submit: u32) -> IoResult<i32> {
        // SAFETY: `sq_off.flags` is a u32-aligned byte offset inside
        // the SQ-ring mapping; the returned reference's lifetime is
        // bounded by `&self` and the mapping outlives the borrow.
        let flags: &AtomicU32 = unsafe { atomic_at(self.sq_ring_ptr, self.params.sq_off.flags) };
        let sq_flags = flags.load(Ordering::Acquire);
        if (sq_flags & IORING_SQ_NEED_WAKEUP) != 0 {
            raw::io_uring_enter(self.fd, to_submit, 0, IORING_ENTER_SQ_WAKEUP)
        } else {
            Ok(to_submit as i32)
        }
    }

    /// Register a file-descriptor table with the ring
    /// (`IORING_REGISTER_FILES`). Subsequent SQEs flagged with
    /// [`Sqe::with_fixed_file`] interpret their `fd` field as an
    /// index into this table.
    ///
    /// Pass an empty slice to register a zero-length table (rare;
    /// useful only as a placeholder).
    ///
    /// [`Sqe::with_fixed_file`]: super::Sqe::with_fixed_file
    pub fn register_files(&self, fds: &[RawFd]) -> IoResult<()> {
        let ptr = if fds.is_empty() {
            0
        } else {
            fds.as_ptr() as usize
        };
        // SAFETY: `fds` lives for the syscall duration; the kernel
        // copies the table into its own storage before returning.
        unsafe {
            raw::io_uring_register(self.fd, IORING_REGISTER_FILES, ptr, fds.len() as u32)
                .map(|_| ())
        }
    }

    /// Drop the registered file table.
    pub fn unregister_files(&self) -> IoResult<()> {
        // SAFETY: `IORING_UNREGISTER_FILES` reads no arg pointer.
        unsafe { raw::io_uring_register(self.fd, IORING_UNREGISTER_FILES, 0, 0).map(|_| ()) }
    }

    /// Register a buffer-iovec table with the ring
    /// (`IORING_REGISTER_BUFFERS`). Pairs with the
    /// fixed-buffer-flavoured read / write ops (kernel >= 5.1) for
    /// the cost of one copy per syscall.
    ///
    /// `bufs` is a sequence of `(ptr, len)` pairs describing pinned
    /// userspace regions; each region must remain valid until the
    /// matching [`Self::unregister_buffers`] (or ring drop).
    ///
    /// # Safety
    ///
    /// Every `(ptr, len)` slice must satisfy
    /// [`core::slice::from_raw_parts_mut`]'s validity contract for
    /// the entire lifetime of the registration. The kernel may write
    /// to any registered region in response to a fixed-buffer read.
    pub unsafe fn register_buffers(&self, bufs: &[(*mut u8, usize)]) -> IoResult<()> {
        // Build an `IoVec` array on the stack-of-allocator's choice;
        // `Vec` works because `no_std + alloc`. The table is freed at
        // the end of this call — the kernel copies it.
        let iovecs: alloc::vec::Vec<IoVec> = bufs
            .iter()
            .map(|(base, len)| IoVec {
                iov_base: *base,
                iov_len: *len,
            })
            .collect();
        let ptr = if iovecs.is_empty() {
            0
        } else {
            iovecs.as_ptr() as usize
        };
        // SAFETY: `iovecs` lives for the syscall duration; kernel
        // copies the table. Caller-side validity of each iov_base /
        // iov_len pair is the documented contract above.
        unsafe {
            raw::io_uring_register(self.fd, IORING_REGISTER_BUFFERS, ptr, iovecs.len() as u32)
                .map(|_| ())
        }
    }

    /// Drop the registered buffer table.
    pub fn unregister_buffers(&self) -> IoResult<()> {
        // SAFETY: `IORING_UNREGISTER_BUFFERS` reads no arg pointer.
        unsafe { raw::io_uring_register(self.fd, IORING_UNREGISTER_BUFFERS, 0, 0).map(|_| ()) }
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
