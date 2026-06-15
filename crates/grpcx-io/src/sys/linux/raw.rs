//! Linux inline syscall layer — no `libc`, no `windows-sys`, no
//! other crate. Every syscall goes through `core::arch::asm!` against
//! the Linux syscall ABI directly.
//!
//! # Supported targets
//!
//! - `x86_64-unknown-linux-*`
//! - `aarch64-unknown-linux-*`
//!
//! Other architectures fail to compile on Linux (intentional in
//! v0.2). FreeBSD / macOS use kqueue and live in separate `sys`
//! sub-trees added in v0.5 / v0.6.

use crate::types::{IoError, IoErrorKind, IoResult};

/// Linux raw file descriptor.
pub type RawFd = i32;

// ============================================================
// Syscall numbers — Linux uapi.
// ============================================================

#[cfg(target_arch = "x86_64")]
pub(crate) mod nr {
    pub(crate) const READ: usize = 0;
    pub(crate) const WRITE: usize = 1;
    pub(crate) const CLOSE: usize = 3;
    pub(crate) const MMAP: usize = 9;
    pub(crate) const MUNMAP: usize = 11;
    pub(crate) const EPOLL_CREATE1: usize = 291;
    pub(crate) const EPOLL_CTL: usize = 233;
    pub(crate) const EPOLL_PWAIT: usize = 281;
    pub(crate) const PIPE2: usize = 293;
    pub(crate) const EVENTFD2: usize = 290;
    pub(crate) const RECVMMSG: usize = 299;
    pub(crate) const SENDMMSG: usize = 307;
    pub(crate) const IO_URING_SETUP: usize = 425;
    pub(crate) const IO_URING_ENTER: usize = 426;
}

#[cfg(target_arch = "aarch64")]
pub(crate) mod nr {
    pub(crate) const READ: usize = 63;
    pub(crate) const WRITE: usize = 64;
    pub(crate) const CLOSE: usize = 57;
    pub(crate) const PIPE2: usize = 59;
    pub(crate) const EVENTFD2: usize = 19;
    // asm-generic batch-datagram numbers — runtime-unverified
    // (no arm64 Linux box); see the batch section below.
    pub(crate) const RECVMMSG: usize = 243;
    pub(crate) const SENDMMSG: usize = 269;
    pub(crate) const EPOLL_CREATE1: usize = 20;
    pub(crate) const EPOLL_CTL: usize = 21;
    pub(crate) const EPOLL_PWAIT: usize = 22;
    // aarch64 inherits the asm-generic table for mmap/munmap.
    pub(crate) const MUNMAP: usize = 215;
    pub(crate) const MMAP: usize = 222;
    // io_uring numbers are identical on x86_64 and aarch64.
    pub(crate) const IO_URING_SETUP: usize = 425;
    pub(crate) const IO_URING_ENTER: usize = 426;
}

// ============================================================
// epoll constants.
// ============================================================

pub(crate) const EPOLL_CLOEXEC: i32 = 0o2000000;
pub(crate) const EPOLL_CTL_ADD: i32 = 1;
pub(crate) const EPOLL_CTL_DEL: i32 = 2;
pub(crate) const EPOLL_CTL_MOD: i32 = 3;

pub(crate) const EPOLLIN: u32 = 0x001;
pub(crate) const EPOLLPRI: u32 = 0x002;
pub(crate) const EPOLLOUT: u32 = 0x004;
pub(crate) const EPOLLERR: u32 = 0x008;
pub(crate) const EPOLLHUP: u32 = 0x010;
pub(crate) const EPOLLRDHUP: u32 = 0x2000;
pub(crate) const EPOLLET: u32 = 1 << 31;
pub(crate) const EPOLLONESHOT: u32 = 1 << 30;

pub(crate) const O_CLOEXEC: i32 = 0o2000000;
pub(crate) const O_NONBLOCK: i32 = 0o4000;

// eventfd2 flags — same numeric values as the matching O_* flags.
/// Close-on-exec for the eventfd.
pub(crate) const EFD_CLOEXEC: i32 = O_CLOEXEC;
/// Non-blocking reads/writes on the eventfd (so draining never blocks).
pub(crate) const EFD_NONBLOCK: i32 = O_NONBLOCK;

// ============================================================
// mmap / munmap constants — `asm-generic/mman-common.h` +
// `asm-generic/mman.h`. Same values on x86_64 and aarch64.
// ============================================================

pub(crate) const PROT_READ: i32 = 0x1;
pub(crate) const PROT_WRITE: i32 = 0x2;
pub(crate) const MAP_SHARED: i32 = 0x01;
pub(crate) const MAP_POPULATE: i32 = 0x8000;

// ============================================================
// io_uring constants — `linux/io_uring.h`.
// ============================================================

/// mmap offset selecting the submission-queue ring.
pub(crate) const IORING_OFF_SQ_RING: i64 = 0;
/// mmap offset selecting the completion-queue ring.
pub(crate) const IORING_OFF_CQ_RING: i64 = 0x8000000;
/// mmap offset selecting the SQE array.
pub(crate) const IORING_OFF_SQES: i64 = 0x10000000;

/// Kernel maps SQ and CQ rings in a single mmap when this feature bit
/// is set in `IoUringParams::features` (kernels >= 5.4).
pub(crate) const IORING_FEAT_SINGLE_MMAP: u32 = 1 << 0;

/// `io_uring_enter` flag: also reap completions, not just submit.
pub(crate) const IORING_ENTER_GETEVENTS: u32 = 1 << 0;

/// io_uring opcode: no-op, used by smoke tests.
pub(crate) const IORING_OP_NOP: u8 = 0;
/// io_uring opcode: `read(fd, buf, len)` equivalent.
pub(crate) const IORING_OP_READ: u8 = 22;
/// io_uring opcode: `write(fd, buf, len)` equivalent.
pub(crate) const IORING_OP_WRITE: u8 = 23;

// ============================================================
// epoll_event layout.
//
// x86_64 packs the struct (`__attribute__((packed))` in the kernel
// header) — 12 bytes total. aarch64 uses natural alignment — 16
// bytes with a 4-byte pad after `events`. Mismatching this corrupts
// the `data` field on the wrong arch.
// ============================================================

#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub(crate) struct EpollEvent {
    pub(crate) events: u32,
    pub(crate) data: u64,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct EpollEvent {
    pub(crate) events: u32,
    _pad: u32,
    pub(crate) data: u64,
}

impl EpollEvent {
    /// Build an `EpollEvent` with the given `events` mask and `data`.
    ///
    /// Hides the per-arch padding field so callers do not need to
    /// know whether the kernel layout is packed (x86_64) or padded
    /// (aarch64).
    #[inline]
    #[cfg(target_arch = "x86_64")]
    pub(crate) const fn new(events: u32, data: u64) -> Self {
        Self { events, data }
    }

    #[inline]
    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn new(events: u32, data: u64) -> Self {
        Self {
            events,
            _pad: 0,
            data,
        }
    }
}

// ============================================================
// Raw syscall helpers — `core::arch::asm!`.
//
// Linux syscall return convention:
//   `-4095..=-1` → error (negated errno)
//   `>= 0`       → success
// ============================================================

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn syscall3(nr: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn syscall4(nr: usize, a1: usize, a2: usize, a3: usize, a4: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn syscall6(
    nr: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) unsafe fn syscall3(nr: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a1 as isize => ret,
            in("x1") a2,
            in("x2") a3,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) unsafe fn syscall4(nr: usize, a1: usize, a2: usize, a3: usize, a4: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a1 as isize => ret,
            in("x1") a2,
            in("x2") a3,
            in("x3") a4,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) unsafe fn syscall6(
    nr: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a1 as isize => ret,
            in("x1") a2,
            in("x2") a3,
            in("x3") a4,
            in("x4") a5,
            in("x5") a6,
            options(nostack, preserves_flags),
        );
    }
    ret
}

// ============================================================
// errno mapping.
// ============================================================

fn errno_to_kind(errno: i32) -> IoErrorKind {
    match errno {
        2 => IoErrorKind::NotFound,            // ENOENT
        4 => IoErrorKind::Interrupted,         // EINTR
        9 => IoErrorKind::InvalidInput,        // EBADF — invalid fd
        11 => IoErrorKind::WouldBlock,         // EAGAIN / EWOULDBLOCK
        12 => IoErrorKind::OutOfMemory,        // ENOMEM
        13 => IoErrorKind::PermissionDenied,   // EACCES
        17 => IoErrorKind::AlreadyRegistered,  // EEXIST — epoll: fd already in set
        22 => IoErrorKind::InvalidInput,       // EINVAL
        111 => IoErrorKind::ConnectionRefused, // ECONNREFUSED (ICMP on connected UDP)
        _ => IoErrorKind::Other,
    }
}

#[inline]
fn from_syscall_ret(ret: isize) -> IoResult<isize> {
    if ret < 0 {
        let errno = (-ret) as i32;
        Err(IoError::new(errno_to_kind(errno), errno))
    } else {
        Ok(ret)
    }
}

// ============================================================
// Safe-ish public wrappers.
// ============================================================

/// `epoll_create1(flags)` — create a new epoll instance.
pub(crate) fn epoll_create1(flags: i32) -> IoResult<RawFd> {
    // SAFETY: epoll_create1 takes a single int flag, no pointers.
    let ret = unsafe { syscall3(nr::EPOLL_CREATE1, flags as usize, 0, 0) };
    from_syscall_ret(ret).map(|r| r as RawFd)
}

/// `epoll_ctl(epfd, op, fd, event)`.
///
/// `event = None` is only legal for `EPOLL_CTL_DEL`; modern kernels
/// (>= 2.6.9) accept it there. Pass `Some(&event)` for `ADD` / `MOD`.
pub(crate) fn epoll_ctl(
    epfd: RawFd,
    op: i32,
    fd: RawFd,
    event: Option<&EpollEvent>,
) -> IoResult<()> {
    let event_ptr = match event {
        Some(e) => core::ptr::from_ref::<EpollEvent>(e) as usize,
        None => 0,
    };
    // SAFETY: when non-null, `event_ptr` borrows valid memory for
    // the syscall duration. epoll_ctl does not retain the pointer.
    let ret = unsafe {
        syscall4(
            nr::EPOLL_CTL,
            epfd as usize,
            op as usize,
            fd as usize,
            event_ptr,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}

/// `epoll_pwait(epfd, events, maxevents, timeout_ms, sigmask=NULL)`.
///
/// `timeout_ms = -1` waits indefinitely; `0` returns immediately.
/// Returns the number of events written into `events`.
pub(crate) fn epoll_pwait(
    epfd: RawFd,
    events: &mut [EpollEvent],
    timeout_ms: i32,
) -> IoResult<usize> {
    // `timeout` is `int` in the kernel ABI. Cast via `isize` first so
    // negative values sign-extend; the kernel reads the low 32 bits.
    let timeout = timeout_ms as isize as usize;

    // SAFETY: `events` is valid for `events.len()` writes for the
    // syscall duration. Sigmask is NULL (size 0).
    let ret = unsafe {
        syscall6(
            nr::EPOLL_PWAIT,
            epfd as usize,
            events.as_mut_ptr() as usize,
            events.len(),
            timeout,
            0, // sigmask = NULL
            0, // sigsetsize = 0
        )
    };
    from_syscall_ret(ret).map(|n| n as usize)
}

/// `close(fd)`.
pub(crate) fn close(fd: RawFd) -> IoResult<()> {
    // SAFETY: takes an integer, no pointer.
    let ret = unsafe { syscall3(nr::CLOSE, fd as usize, 0, 0) };
    from_syscall_ret(ret).map(|_| ())
}

/// `write(fd, buf)` — exposed for conformance tests in T10.
pub(crate) fn write(fd: RawFd, buf: &[u8]) -> IoResult<usize> {
    // SAFETY: `buf` lives for the syscall duration. write takes a
    // pointer + length and does not retain either.
    let ret = unsafe { syscall3(nr::WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) };
    from_syscall_ret(ret).map(|n| n as usize)
}

/// `read(fd, buf)` — exposed for conformance / bench helpers.
pub(crate) fn read(fd: RawFd, buf: &mut [u8]) -> IoResult<usize> {
    // SAFETY: `buf` is valid for `buf.len()` writes for the syscall
    // duration. read takes a pointer + length, does not retain.
    let ret = unsafe { syscall3(nr::READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) };
    from_syscall_ret(ret).map(|n| n as usize)
}

/// `pipe2(pipefd, flags)` — exposed for conformance tests in T10.
pub(crate) fn pipe2(flags: i32) -> IoResult<(RawFd, RawFd)> {
    let mut fds: [RawFd; 2] = [0; 2];
    // SAFETY: `fds` points to two valid `i32` slots for the syscall
    // duration; pipe2 writes both before returning.
    let ret = unsafe { syscall3(nr::PIPE2, fds.as_mut_ptr() as usize, flags as usize, 0) };
    from_syscall_ret(ret).map(|_| (fds[0], fds[1]))
}

/// `eventfd2(initval, flags)` — create an eventfd counter object.
///
/// Used by the cross-thread [`Waker`](crate::Waker): one `write` of a
/// `u64` makes it readable; a `read` drains the counter back to zero.
pub(crate) fn eventfd(initval: u32, flags: i32) -> IoResult<RawFd> {
    // SAFETY: scalar arguments only; no pointer is passed.
    let ret = unsafe { syscall3(nr::EVENTFD2, initval as usize, flags as usize, 0) };
    from_syscall_ret(ret).map(|r| r as RawFd)
}

// ============================================================
// io_uring kernel ABI structs — `linux/io_uring.h`.
//
// The handoff layer wraps the bare structs only — encoding SQEs and
// reaping CQEs lives in `sys::linux::io_uring`.
// ============================================================

/// Byte offsets into the SQ-ring mmap region for each control field.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct IoSqringOffsets {
    pub(crate) head: u32,
    pub(crate) tail: u32,
    pub(crate) ring_mask: u32,
    pub(crate) ring_entries: u32,
    pub(crate) flags: u32,
    pub(crate) dropped: u32,
    pub(crate) array: u32,
    pub(crate) resv1: u32,
    pub(crate) resv2: u64,
}

/// Byte offsets into the CQ-ring mmap region for each control field.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct IoCqringOffsets {
    pub(crate) head: u32,
    pub(crate) tail: u32,
    pub(crate) ring_mask: u32,
    pub(crate) ring_entries: u32,
    pub(crate) overflow: u32,
    pub(crate) cqes: u32,
    pub(crate) flags: u32,
    pub(crate) resv1: u32,
    pub(crate) resv2: u64,
}

/// Setup parameters passed in and out of `io_uring_setup`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct IoUringParams {
    pub(crate) sq_entries: u32,
    pub(crate) cq_entries: u32,
    pub(crate) flags: u32,
    pub(crate) sq_thread_cpu: u32,
    pub(crate) sq_thread_idle: u32,
    pub(crate) features: u32,
    pub(crate) wq_fd: u32,
    pub(crate) resv: [u32; 3],
    pub(crate) sq_off: IoSqringOffsets,
    pub(crate) cq_off: IoCqringOffsets,
}

/// Completion-queue entry — 16 bytes, the default ring CQE layout.
///
/// The kernel writes a CQE for each completed SQE; `user_data`
/// echoes the SQE's `user_data` so the caller can route the result.
/// `res` is the syscall-style return: non-negative on success
/// (bytes / handle / etc.), negative `-errno` on failure.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IoUringCqe {
    pub(crate) user_data: u64,
    pub(crate) res: i32,
    pub(crate) flags: u32,
}

/// Submission-queue entry — 64 bytes, the default ring SQE layout.
///
/// Fields cover the union'd view in `linux/io_uring.h`: the kernel
/// reinterprets some slots based on `opcode`, but the bytes are
/// always at the same offsets. Builders in `io_uring::sqe` fill the
/// relevant fields per opcode; uninvolved fields stay zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct IoUringSqe {
    pub(crate) opcode: u8,
    pub(crate) flags: u8,
    pub(crate) ioprio: u16,
    pub(crate) fd: i32,
    /// Union: `off` / `addr2` / `cmd_op+pad1` per opcode.
    pub(crate) off: u64,
    /// Union: `addr` / `splice_off_in` / `level+optname` per opcode.
    pub(crate) addr: u64,
    pub(crate) len: u32,
    /// Union: `rw_flags` / `poll_events` / `*_flags` per opcode.
    pub(crate) op_flags: u32,
    pub(crate) user_data: u64,
    /// Union: `buf_index` / `buf_group` per opcode.
    pub(crate) buf_index: u16,
    pub(crate) personality: u16,
    /// Union: `splice_fd_in` / `file_index` / `optlen` per opcode.
    pub(crate) splice_fd_in: i32,
    /// Union: `addr3` / `optval` / `cmd[0]` per opcode.
    pub(crate) addr3: u64,
    pub(crate) _pad2: u64,
}

// Compile-time guards against kernel-ABI drift. Sizes are fixed by
// `linux/io_uring.h`; if the Rust layout ever deviates the build
// fails immediately rather than corrupting kernel state at runtime.
const _: () = assert!(core::mem::size_of::<IoSqringOffsets>() == 40);
const _: () = assert!(core::mem::size_of::<IoCqringOffsets>() == 40);
const _: () = assert!(core::mem::size_of::<IoUringParams>() == 120);
const _: () = assert!(core::mem::size_of::<IoUringSqe>() == 64);
const _: () = assert!(core::mem::size_of::<IoUringCqe>() == 16);

// ============================================================
// io_uring + mmap raw wrappers.
// ============================================================

/// `io_uring_setup(entries, params)` — create a ring, fill `params`.
///
/// `entries` is rounded up to a power of two by the kernel. On
/// success returns the ring fd; on `ENOSYS` / `EINVAL` (kernels
/// < 5.1, or unsupported flags) the error surfaces with the raw
/// errno preserved.
pub(crate) fn io_uring_setup(entries: u32, params: &mut IoUringParams) -> IoResult<RawFd> {
    // SAFETY: `params` is a valid mutable reference for the syscall
    // duration. The kernel reads + writes the struct; no pointer is
    // retained beyond the call.
    let ret = unsafe {
        syscall3(
            nr::IO_URING_SETUP,
            entries as usize,
            core::ptr::from_mut::<IoUringParams>(params) as usize,
            0,
        )
    };
    from_syscall_ret(ret).map(|r| r as RawFd)
}

/// `io_uring_enter(fd, to_submit, min_complete, flags, NULL, 0)`.
///
/// Submits up to `to_submit` SQEs and (when `IORING_ENTER_GETEVENTS`
/// is set) blocks until at least `min_complete` CQEs are available.
/// Returns the number of SQEs actually consumed by the kernel.
pub(crate) fn io_uring_enter(
    fd: RawFd,
    to_submit: u32,
    min_complete: u32,
    flags: u32,
) -> IoResult<i32> {
    // SAFETY: sigset pointer is NULL, sigsetsize is 0.
    let ret = unsafe {
        syscall6(
            nr::IO_URING_ENTER,
            fd as usize,
            to_submit as usize,
            min_complete as usize,
            flags as usize,
            0,
            0,
        )
    };
    from_syscall_ret(ret).map(|n| n as i32)
}

/// `mmap(addr, len, prot, flags, fd, offset)`.
///
/// Returns the mapped base address on success; a Linux user-space
/// address always fits in positive `isize`, so the errno-range check
/// in [`from_syscall_ret`] is sound here.
pub(crate) fn mmap(
    addr: usize,
    len: usize,
    prot: i32,
    flags: i32,
    fd: RawFd,
    offset: i64,
) -> IoResult<*mut u8> {
    // SAFETY: all arguments are scalars; no userspace pointer is
    // dereferenced by mmap itself. Caller owns the resulting region.
    let ret = unsafe {
        syscall6(
            nr::MMAP,
            addr,
            len,
            prot as usize,
            flags as usize,
            fd as usize,
            offset as usize,
        )
    };
    from_syscall_ret(ret).map(|p| p as *mut u8)
}

/// `munmap(addr, len)`.
pub(crate) fn munmap(addr: *mut u8, len: usize) -> IoResult<()> {
    // SAFETY: scalar arguments; the kernel verifies the range. Caller
    // promises `addr`/`len` cover a region they previously mapped.
    let ret = unsafe { syscall3(nr::MUNMAP, addr as usize, len, 0) };
    from_syscall_ret(ret).map(|_| ())
}

// ============================================================
// Batch datagram syscalls (`sendmmsg` / `recvmmsg`).
//
// Same contract as the Darwin `sendmsg_x` / `recvmsg_x` pair:
// CONNECTED non-blocking UDP sockets only (callers never
// populate `msg_name`), ≤64 datagrams per syscall, short
// count on send-buffer pressure, `WouldBlock` when zero
// move. flags = 0 — non-blocking behaviour comes from the
// socket's O_NONBLOCK, `recvmmsg` timeout = NULL.
//
// x86_64 numbers hw-verified on a Debian 13 / 6.12 box by
// loopback round-trip KATs (tests/udp_batch_linux.rs);
// aarch64 numbers come from the asm-generic table (same
// source as the epoll/eventfd numbers above) — runtime-
// unverified, recorded in REFACTOR-v10-linux-sendmmsg.md.
// ============================================================

/// `struct iovec` — LP64.
#[repr(C)]
#[derive(Clone, Copy)]
struct Iovec {
    iov_base: *mut u8,
    iov_len: usize,
}

/// `struct msghdr` — LP64 (x86_64 and aarch64 share the
/// layout: 4 bytes implicit padding after `msg_namelen`,
/// 4 after `msg_flags`).
#[repr(C)]
struct Msghdr {
    msg_name: *mut u8,
    msg_namelen: u32,
    msg_iov: *mut Iovec,
    msg_iovlen: usize,
    msg_control: *mut u8,
    msg_controllen: usize,
    msg_flags: i32,
}

/// `struct mmsghdr` — one batch entry; the kernel writes the
/// per-message byte count into `msg_len`.
#[repr(C)]
struct Mmsghdr {
    msg_hdr: Msghdr,
    msg_len: u32,
}

impl Mmsghdr {
    fn zeroed() -> Self {
        // SAFETY: all-zero is a valid bit pattern for every
        // field (null pointers + zero lengths).
        unsafe { core::mem::zeroed() }
    }
}

/// Send up to `datagrams.len()` datagrams on a CONNECTED
/// non-blocking UDP socket in one syscall. Returns how many
/// were accepted (can be short on send-buffer pressure;
/// `WouldBlock` when zero fit).
pub(crate) fn sendmmsg(fd: RawFd, datagrams: &[&[u8]]) -> IoResult<usize> {
    const CAP: usize = 64;
    let n = datagrams.len().min(CAP);
    let mut iovs = [Iovec {
        iov_base: core::ptr::null_mut(),
        iov_len: 0,
    }; CAP];
    let mut hdrs: [Mmsghdr; CAP] = core::array::from_fn(|_| Mmsghdr::zeroed());
    for (i, d) in datagrams[..n].iter().enumerate() {
        iovs[i] = Iovec {
            iov_base: d.as_ptr() as *mut u8,
            iov_len: d.len(),
        };
        hdrs[i].msg_hdr.msg_iov = &mut iovs[i];
        hdrs[i].msg_hdr.msg_iovlen = 1;
    }
    // SAFETY: `hdrs[..n]` points at live iovecs over live
    // borrows for the duration of the call; vlen = n.
    let ret = unsafe { syscall4(nr::SENDMMSG, fd as usize, hdrs.as_mut_ptr() as usize, n, 0) };
    from_syscall_ret(ret).map(|v| v as usize)
}

/// Receive up to `bufs.len()` datagrams on a CONNECTED
/// non-blocking UDP socket in one syscall. Per-datagram
/// lengths land in `lens`; returns how many arrived
/// (`WouldBlock` when none queued).
pub(crate) fn recvmmsg(fd: RawFd, bufs: &mut [&mut [u8]], lens: &mut [usize]) -> IoResult<usize> {
    const CAP: usize = 64;
    let n = bufs.len().min(lens.len()).min(CAP);
    let mut iovs = [Iovec {
        iov_base: core::ptr::null_mut(),
        iov_len: 0,
    }; CAP];
    let mut hdrs: [Mmsghdr; CAP] = core::array::from_fn(|_| Mmsghdr::zeroed());
    for (i, b) in bufs[..n].iter_mut().enumerate() {
        iovs[i] = Iovec {
            iov_base: b.as_mut_ptr(),
            iov_len: b.len(),
        };
        hdrs[i].msg_hdr.msg_iov = &mut iovs[i];
        hdrs[i].msg_hdr.msg_iovlen = 1;
    }
    // SAFETY: as above; timeout = NULL (arg 5), the kernel
    // writes each datagram's byte count into `msg_len`.
    let ret = unsafe {
        syscall6(
            nr::RECVMMSG,
            fd as usize,
            hdrs.as_mut_ptr() as usize,
            n,
            0,
            0,
            0,
        )
    };
    let got = from_syscall_ret(ret)? as usize;
    for (i, l) in lens[..got.min(n)].iter_mut().enumerate() {
        *l = hdrs[i].msg_len as usize;
    }
    Ok(got)
}
