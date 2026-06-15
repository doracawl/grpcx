//! FreeBSD inline syscall layer — no `libc`, no other crate. Every
//! syscall goes through `core::arch::asm!` against the FreeBSD
//! amd64 syscall ABI directly.
//!
//! # Supported targets
//!
//! - `x86_64-unknown-freebsd` (amd64 FreeBSD)
//!
//! # ABI shape (vs Linux and Darwin)
//!
//! 1. **Bare syscall numbers** (no Darwin-style class prefix). FreeBSD
//!    syscalls are flat unsigned integers; rax holds the number
//!    directly. Numbers are from `/usr/include/sys/syscall.h`.
//! 2. **Linux-style register convention** on amd64: args in
//!    `rdi/rsi/rdx/r10/r8/r9`, syscall via `syscall` instruction.
//! 3. **Darwin-style carry-flag error path**: CF=1 indicates error,
//!    `rax` carries a positive `errno`. Linux's `-errno` shortcut is
//!    wrong here. Returns are surfaced as `(value, error_flag)`
//!    tuples and consumed via [`from_syscall_ret`].
//! 4. **`pipe2` available natively** (`SYS_pipe2 = 542`). Unlike
//!    Darwin, FreeBSD has a real `pipe2` syscall — one call sets
//!    `O_CLOEXEC` atomically, no follow-up `fcntl` needed.
//! 5. **`kevent` struct is 64 bytes** (FreeBSD 12+). The leading 32
//!    bytes match Darwin's layout (ident / filter / flags / fflags /
//!    data / udata); FreeBSD adds an `ext[4]` tail for filter-extra
//!    data. We compile-time assert the size.
//! 6. **Extra clobbers vs Linux/Darwin**: FreeBSD's amd64 syscall
//!    handler uses `r8` / `r9` / `r10` as scratch and does **not**
//!    restore them on return (it moves `r10 → rcx` for the C-ABI
//!    arg4 slot and uses `r8` / `r9` similarly during arg
//!    marshalling). Linux preserves all three; Darwin preserves
//!    them on x86_64. Our asm therefore declares `r8`, `r9`, `r10`
//!    as clobbered on every FreeBSD syscall — without this the
//!    optimizer keeps live values in those registers across the
//!    asm and segfaults when they get corrupted by the kernel
//!    return path. Discovered empirically via release-mode
//!    SIGSEGV in v0.7 conformance; debug builds never schedule
//!    those registers across syscalls so the bug stayed dormant
//!    until release.

use crate::types::{IoError, IoErrorKind, IoResult};

/// FreeBSD raw file descriptor.
pub type RawFd = i32;

// ============================================================
// FreeBSD syscall numbers (amd64 — from `<sys/syscall.h>` on 15.0).
// ============================================================

pub(crate) mod nr {
    pub(crate) const READ: usize = 3;
    pub(crate) const WRITE: usize = 4;
    pub(crate) const CLOSE: usize = 6;
    pub(crate) const FCNTL: usize = 92;
    pub(crate) const KQUEUE: usize = 362;
    /// `SYS_pipe2` — modern `pipe2(int fds[2], int flags)`. FreeBSD
    /// 10+; we target 12+ for the 64-byte `kevent` so this is always
    /// available.
    pub(crate) const PIPE2: usize = 542;
    /// `SYS_kevent` — modern 64-byte struct (FreeBSD 12+). The
    /// legacy `freebsd11_kevent` (32-byte struct, syscall 363) is
    /// not used.
    pub(crate) const KEVENT: usize = 560;
}

// ============================================================
// fcntl + open-flag constants — FreeBSD headers (`<sys/fcntl.h>`).
// ============================================================

/// `fcntl` command: set file descriptor flags.
pub(crate) const F_SETFD: i32 = 2;
/// `fcntl` command: get file status flags.
pub(crate) const F_GETFL: i32 = 3;
/// `fcntl` command: set file status flags.
pub(crate) const F_SETFL: i32 = 4;
/// Close-on-exec bit value.
pub(crate) const FD_CLOEXEC: i32 = 1;
/// `O_CLOEXEC` open-flag — passed to `pipe2`.
pub(crate) const O_CLOEXEC: i32 = 0x00100000;
/// Non-blocking mode flag for `F_SETFL`.
pub(crate) const O_NONBLOCK: i32 = 0x0004;

// ============================================================
// Raw syscall helpers — FreeBSD signals errors via the CARRY FLAG
// (positive errno when set). The helper returns a
// (value, error_flag) tuple; consumers feed it through
// [`from_syscall_ret`] to get an `IoResult`.
// ============================================================

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn syscall3(nr: usize, a1: usize, a2: usize, a3: usize) -> (isize, u8) {
    let val: isize;
    let err: u8;
    unsafe {
        core::arch::asm!(
            "syscall",
            "setc {err}",
            inlateout("rax") nr as isize => val,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("r10") _,
            lateout("r11") _,
            err = lateout(reg_byte) err,
            options(nostack),
        );
    }
    (val, err)
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
) -> (isize, u8) {
    let val: isize;
    let err: u8;
    unsafe {
        // r8/r9/r10 carry args 5/6/4 going in; FreeBSD kernel does
        // not restore them, so they're marked as `inout(...) => _`
        // — the optimizer must treat them as garbage on return.
        core::arch::asm!(
            "syscall",
            "setc {err}",
            inlateout("rax") nr as isize => val,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            inout("r10") a4 => _,
            inout("r8") a5 => _,
            inout("r9") a6 => _,
            lateout("rcx") _,
            lateout("r11") _,
            err = lateout(reg_byte) err,
            options(nostack),
        );
    }
    (val, err)
}

// ============================================================
// errno mapping — FreeBSD numbers match Darwin's BSD heritage
// (notably `EAGAIN = 35`, identical to Darwin, distinct from
// Linux's 11).
// ============================================================

fn errno_to_kind(errno: i32) -> IoErrorKind {
    match errno {
        2 => IoErrorKind::NotFound,           // ENOENT
        4 => IoErrorKind::Interrupted,        // EINTR
        9 => IoErrorKind::InvalidInput,       // EBADF
        12 => IoErrorKind::OutOfMemory,       // ENOMEM
        13 => IoErrorKind::PermissionDenied,  // EACCES
        17 => IoErrorKind::AlreadyRegistered, // EEXIST
        22 => IoErrorKind::InvalidInput,      // EINVAL
        35 => IoErrorKind::WouldBlock,        // EAGAIN
        61 => IoErrorKind::ConnectionRefused, // ECONNREFUSED (BSD value, same as Darwin)
        _ => IoErrorKind::Other,
    }
}

/// Consume the `(value, error_flag)` tuple from a `syscall*` helper.
#[inline]
pub(crate) fn from_syscall_ret(pair: (isize, u8)) -> IoResult<isize> {
    let (val, err) = pair;
    if err != 0 {
        let errno = val as i32;
        Err(IoError::new(errno_to_kind(errno), errno))
    } else {
        Ok(val)
    }
}

// ============================================================
// Safe-ish public wrappers.
// ============================================================

/// `close(fd)`.
pub(crate) fn close(fd: RawFd) -> IoResult<()> {
    // SAFETY: scalar argument.
    let r = unsafe { syscall3(nr::CLOSE, fd as usize, 0, 0) };
    from_syscall_ret(r).map(|_| ())
}

/// `read(fd, buf)` — exposed for conformance / bench helpers.
pub(crate) fn read(fd: RawFd, buf: &mut [u8]) -> IoResult<usize> {
    // SAFETY: `buf` is valid for `buf.len()` writes for the syscall
    // duration. The kernel does not retain the pointer.
    let r = unsafe { syscall3(nr::READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) };
    from_syscall_ret(r).map(|n| n as usize)
}

/// `write(fd, buf)` — exposed for conformance / bench helpers.
pub(crate) fn write(fd: RawFd, buf: &[u8]) -> IoResult<usize> {
    // SAFETY: `buf` lives for the syscall duration.
    let r = unsafe { syscall3(nr::WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) };
    from_syscall_ret(r).map(|n| n as usize)
}

/// `fcntl(fd, cmd, arg)`.
pub(crate) fn fcntl(fd: RawFd, cmd: i32, arg: i32) -> IoResult<i32> {
    // SAFETY: scalar arguments.
    let r = unsafe { syscall3(nr::FCNTL, fd as usize, cmd as usize, arg as usize) };
    from_syscall_ret(r).map(|n| n as i32)
}

/// `pipe2(fds, O_CLOEXEC)` — one atomic syscall on FreeBSD. The
/// two fds are written into the caller-supplied `[i32; 2]` array;
/// `O_CLOEXEC` is set atomically (no race with `exec*` from
/// another thread, unlike the Darwin `pipe + fcntl` two-step).
pub(crate) fn pipe_cloexec() -> IoResult<(RawFd, RawFd)> {
    let mut fds: [RawFd; 2] = [0, 0];
    // SAFETY: `fds` is valid for the syscall duration.
    let r = unsafe { syscall3(nr::PIPE2, fds.as_mut_ptr() as usize, O_CLOEXEC as usize, 0) };
    from_syscall_ret(r)?;
    Ok((fds[0], fds[1]))
}

// ============================================================
// kqueue / kevent — `<sys/event.h>`. FreeBSD 12+ struct is 64
// bytes: leading 32 bytes match Darwin, then a 32-byte `ext[4]`
// tail for filter-extra data. Only the leading fields are
// surfaced to the L1 API; `ext` round-trips opaquely.
// ============================================================

/// One kqueue event — FreeBSD 12+ shape (64 bytes).
///
/// The first 6 fields mirror Darwin's [`crate::sys::darwin::Kevent`]
/// exactly, so the same `kevent_to_ready` / `token_from_event`
/// translation logic applies. The trailing `ext[4]` is unused by
/// the L1 read/write/exception abstraction but must be present so
/// the struct size matches the kernel's expectation.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Kevent {
    /// Watched identifier — usually an `fd` for fd-bound filters.
    pub(crate) ident: usize,
    /// Filter selector — negative `i16` constants
    /// ([`EVFILT_READ`], [`EVFILT_WRITE`]).
    pub(crate) filter: i16,
    /// Action / state flags ([`EV_ADD`], [`EV_DELETE`],
    /// [`EV_CLEAR`], ...).
    pub(crate) flags: u16,
    /// Filter-specific flags (mostly 0 at L1).
    pub(crate) fflags: u32,
    /// Filter-specific data (e.g. number of bytes available for
    /// `EVFILT_READ`).
    pub(crate) data: isize,
    /// Opaque user cookie — round-trip from the change list to the
    /// returned event. Stored as `usize` since the kernel never
    /// dereferences it.
    pub(crate) udata: usize,
    /// FreeBSD-specific extra-data slots. Unused at L1; the kernel
    /// must see this storage as part of the struct.
    pub(crate) ext: [u64; 4],
}

const _: () = assert!(core::mem::size_of::<Kevent>() == 64);

/// Filter: read readiness on an fd.
pub(crate) const EVFILT_READ: i16 = -1;
/// Filter: write readiness on an fd.
pub(crate) const EVFILT_WRITE: i16 = -2;
// Note: FreeBSD has no `EVFILT_EXCEPT`. `Interest::PRIORITY` is
// silently dropped on FreeBSD — see `kqueue::interest_to_changes`.
/// Filter: user-triggered event — the cross-thread [`Waker`](crate::Waker)
/// backend. FreeBSD's value is `-11` (differs from Darwin's `-10`).
pub(crate) const EVFILT_USER: i16 = -11;
/// `fflags` bit that fires a registered `EVFILT_USER` event.
pub(crate) const NOTE_TRIGGER: u32 = 0x0100_0000;

/// Add the event to the kqueue (creates if not present).
pub(crate) const EV_ADD: u16 = 0x0001;
/// Remove the event from the kqueue.
pub(crate) const EV_DELETE: u16 = 0x0002;
/// Re-enable a previously disabled event.
pub(crate) const EV_ENABLE: u16 = 0x0004;
/// Disable but keep the event registered.
pub(crate) const EV_DISABLE: u16 = 0x0008;
/// Delete the event after one fire.
pub(crate) const EV_ONESHOT: u16 = 0x0010;
/// Edge-triggered semantics — match mio's behavior for cross-OS
/// parity.
pub(crate) const EV_CLEAR: u16 = 0x0020;
/// Force a return path for the change list entry without
/// delivering any waiting events (used for batched change
/// diagnostics).
pub(crate) const EV_RECEIPT: u16 = 0x0040;
/// Kernel signals EOF on a half-closed connection.
pub(crate) const EV_EOF: u16 = 0x8000;
/// Kernel signals an error inline in this event slot.
pub(crate) const EV_ERROR: u16 = 0x4000;

/// `timespec` for `kevent` timeouts. Same layout as Darwin's, since
/// both are amd64 64-bit BSD-derived.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Timespec {
    pub(crate) tv_sec: isize,
    pub(crate) tv_nsec: isize,
}

const _: () = assert!(core::mem::size_of::<Timespec>() == 16);

/// `kqueue()` — create a new kqueue and return its fd.
pub(crate) fn kqueue() -> IoResult<RawFd> {
    // SAFETY: takes no arguments.
    let r = unsafe { syscall3(nr::KQUEUE, 0, 0, 0) };
    from_syscall_ret(r).map(|n| n as RawFd)
}

/// `kevent(kq, changelist, nchanges, eventlist, nevents, timeout)`
/// — apply pending changes and (optionally) wait for events.
///
/// `timeout = None` waits indefinitely; `Some(&Timespec{0,0})`
/// returns immediately.
pub(crate) fn kevent(
    kq: RawFd,
    changelist: &[Kevent],
    eventlist: &mut [Kevent],
    timeout: Option<&Timespec>,
) -> IoResult<usize> {
    let timeout_ptr = match timeout {
        Some(t) => core::ptr::from_ref::<Timespec>(t) as usize,
        None => 0,
    };
    // SAFETY: every pointer is valid for the syscall duration; the
    // kernel does not retain them across the call.
    let r = unsafe {
        syscall6(
            nr::KEVENT,
            kq as usize,
            changelist.as_ptr() as usize,
            changelist.len(),
            eventlist.as_mut_ptr() as usize,
            eventlist.len(),
            timeout_ptr,
        )
    };
    from_syscall_ret(r).map(|n| n as usize)
}
