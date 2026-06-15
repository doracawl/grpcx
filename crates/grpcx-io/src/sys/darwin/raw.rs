//! Darwin (macOS / iOS) inline syscall layer — no `libc`, no other
//! crate. Every syscall goes through `core::arch::asm!` against
//! the Darwin syscall ABI directly.
//!
//! # Supported targets
//!
//! - `x86_64-apple-darwin` (Intel Mac)
//! - `aarch64-apple-darwin` (Apple Silicon Mac)
//! - `aarch64-apple-ios` and `*-ios-sim` (iOS device + simulator)
//!
//! # ABI differences from Linux
//!
//! 1. **x86_64 — class-tagged numbers**. Darwin packs `(class << 24)
//!    | unix_num` into `rax`. The BSD-class prefix is `2 << 24 =
//!    0x2000000`. Calling with a bare unix number lands in a
//!    different syscall class (Mach / machdep / ...) and faults.
//! 2. **aarch64 — `svc #0x80` + `x16`**. Apple Silicon uses
//!    `svc #0x80` (not Linux's `svc #0`) and puts the syscall
//!    number in `x16` (not Linux's `x8`).
//! 3. **Carry-flag error path**. Both arches signal error via the
//!    carry flag (CF on x86, PSTATE.C on arm); `x0`/`rax` carries
//!    a positive errno. Linux's `-errno` shortcut is wrong here.

use crate::types::{IoError, IoErrorKind, IoResult};

/// Darwin raw file descriptor.
pub type RawFd = i32;

/// Darwin BSD-class prefix on x86_64 — `2 << 24`.
#[cfg(target_arch = "x86_64")]
const CLASS_UNIX: usize = 2 << 24;

#[cfg(target_arch = "x86_64")]
#[inline]
const fn sys_no(unix: usize) -> usize {
    CLASS_UNIX | unix
}

/// On aarch64 Darwin the BSD class lives in the syscall-instruction
/// selector (`svc #0x80`), not in the number — so `x16` holds the
/// bare unix number.
#[cfg(target_arch = "aarch64")]
#[inline]
const fn sys_no(unix: usize) -> usize {
    unix
}

// ============================================================
// Darwin syscall numbers (BSD subset — from `bsd/sys/syscall.h`).
// ============================================================

pub(crate) mod nr {
    pub(crate) const READ: usize = 3;
    pub(crate) const WRITE: usize = 4;
    pub(crate) const CLOSE: usize = 6;
    pub(crate) const PIPE: usize = 42;
    pub(crate) const FCNTL: usize = 92;
    pub(crate) const KQUEUE: usize = 362;
    pub(crate) const KEVENT: usize = 363;
    /// `recvmsg_x(int s, struct msghdr_x *msgp, u_int cnt,
    /// int flags)` — batch datagram receive (XNU-private
    /// but ABI-stable; number from the macOS SDK
    /// `sys/syscall.h`).
    pub(crate) const RECVMSG_X: usize = 480;
    /// `sendmsg_x(...)` — batch datagram send (same
    /// provenance).
    pub(crate) const SENDMSG_X: usize = 481;
}

// ============================================================
// Batch-datagram structures (`struct msghdr_x` — XNU
// `bsd/sys/socket.h` private section; layout is the public
// `msghdr` plus a trailing `msg_datalen`).
// ============================================================

/// `struct iovec` (Darwin, 64-bit).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Iovec {
    pub(crate) iov_base: *mut u8,
    pub(crate) iov_len: usize,
}

/// `struct msghdr_x` (Darwin, 64-bit).
#[repr(C)]
pub(crate) struct MsghdrX {
    pub(crate) msg_name: *mut u8,
    pub(crate) msg_namelen: u32,
    pub(crate) msg_iov: *mut Iovec,
    pub(crate) msg_iovlen: i32,
    pub(crate) msg_control: *mut u8,
    pub(crate) msg_controllen: u32,
    pub(crate) msg_flags: i32,
    /// Out-param on receive: byte length of the datagram.
    pub(crate) msg_datalen: usize,
}

impl MsghdrX {
    pub(crate) fn zeroed() -> Self {
        MsghdrX {
            msg_name: core::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: core::ptr::null_mut(),
            msg_iovlen: 0,
            msg_control: core::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
            msg_datalen: 0,
        }
    }
}

// ============================================================
// fcntl + open-flag constants — Darwin headers (`<fcntl.h>`).
// ============================================================

/// `fcntl(fd, F_SETFD, FD_CLOEXEC)` clears the close-on-exec bit.
pub(crate) const F_SETFD: i32 = 2;
/// Close-on-exec bit value.
pub(crate) const FD_CLOEXEC: i32 = 1;
/// `fcntl` command: get file status flags.
pub(crate) const F_GETFL: i32 = 3;
/// `fcntl` command: set file status flags.
pub(crate) const F_SETFL: i32 = 4;
/// Non-blocking mode flag for `F_SETFL`.
pub(crate) const O_NONBLOCK: i32 = 4;

// ============================================================
// Raw syscall helpers — Darwin uses the CARRY FLAG to signal an
// error path (positive errno when set). The helper returns a
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
            inlateout("rax") sys_no(nr) as isize => val,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
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
        core::arch::asm!(
            "syscall",
            "setc {err}",
            inlateout("rax") sys_no(nr) as isize => val,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rcx") _,
            lateout("r11") _,
            err = lateout(reg_byte) err,
            options(nostack),
        );
    }
    (val, err)
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) unsafe fn syscall3(nr: usize, a1: usize, a2: usize, a3: usize) -> (isize, u8) {
    let val: isize;
    let err: usize;
    unsafe {
        core::arch::asm!(
            "svc #0x80",
            "cset {err}, cs",
            in("x16") sys_no(nr),
            inlateout("x0") a1 as isize => val,
            in("x1") a2,
            in("x2") a3,
            err = lateout(reg) err,
            options(nostack),
        );
    }
    (val, err as u8)
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
) -> (isize, u8) {
    let val: isize;
    let err: usize;
    unsafe {
        core::arch::asm!(
            "svc #0x80",
            "cset {err}, cs",
            in("x16") sys_no(nr),
            inlateout("x0") a1 as isize => val,
            in("x1") a2,
            in("x2") a3,
            in("x3") a4,
            in("x4") a5,
            in("x5") a6,
            err = lateout(reg) err,
            options(nostack),
        );
    }
    (val, err as u8)
}

// ============================================================
// errno mapping — Darwin numbers differ from Linux in several
// places (notably `EAGAIN = 35` on Darwin vs `11` on Linux).
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
        35 => IoErrorKind::WouldBlock,        // EAGAIN (Darwin)
        61 => IoErrorKind::ConnectionRefused, // ECONNREFUSED
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

/// `pipe()` — Darwin returns the two fds via the register pair
/// `(rax, rdx)` on x86_64 and `(x0, x1)` on aarch64. Linux's
/// caller-supplied `int[2]` shape does not apply here.
///
/// Sets `FD_CLOEXEC` on both ends via `fcntl` after construction
/// so we get the same close-on-exec semantics as Linux's
/// `pipe2(O_CLOEXEC)`.
#[cfg(target_arch = "x86_64")]
pub(crate) fn pipe_cloexec() -> IoResult<(RawFd, RawFd)> {
    let read_fd: isize;
    let write_fd: isize;
    let err: u8;
    // SAFETY: `pipe()` takes no arguments; the kernel writes the
    // read-end into rax and the write-end into rdx.
    unsafe {
        core::arch::asm!(
            "syscall",
            "setc {err}",
            inlateout("rax") sys_no(nr::PIPE) as isize => read_fd,
            lateout("rdx") write_fd,
            lateout("rcx") _,
            lateout("r11") _,
            err = lateout(reg_byte) err,
            options(nostack),
        );
    }
    set_cloexec_pair(
        from_syscall_ret((read_fd, err))? as RawFd,
        write_fd as RawFd,
    )
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn pipe_cloexec() -> IoResult<(RawFd, RawFd)> {
    let read_fd: isize;
    let write_fd: isize;
    let err: usize;
    // SAFETY: see x86_64 variant above; aarch64 puts the second
    // return value in x1.
    unsafe {
        core::arch::asm!(
            "svc #0x80",
            "cset {err}, cs",
            in("x16") sys_no(nr::PIPE),
            lateout("x0") read_fd,
            lateout("x1") write_fd,
            err = lateout(reg) err,
            options(nostack),
        );
    }
    set_cloexec_pair(
        from_syscall_ret((read_fd, err as u8))? as RawFd,
        write_fd as RawFd,
    )
}

/// Apply `FD_CLOEXEC` to both fds; closes them and returns the
/// failing fcntl's error if either set fails.
fn set_cloexec_pair(r: RawFd, w: RawFd) -> IoResult<(RawFd, RawFd)> {
    if let Err(e) = fcntl(r, F_SETFD, FD_CLOEXEC) {
        let _ = close(r);
        let _ = close(w);
        return Err(e);
    }
    if let Err(e) = fcntl(w, F_SETFD, FD_CLOEXEC) {
        let _ = close(r);
        let _ = close(w);
        return Err(e);
    }
    Ok((r, w))
}

// ============================================================
// kqueue / kevent — `<sys/event.h>`. Same struct layout on x86_64
// and aarch64 (both are 64-bit; pointers + intptr_t are 8 bytes).
// ============================================================

/// One kqueue event — submission-and-completion element.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Kevent {
    /// Watched identifier — usually an `fd` for fd-bound filters.
    pub(crate) ident: usize,
    /// Filter selector — negative `i16` constants
    /// ([`EVFILT_READ`], [`EVFILT_WRITE`], ...).
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
}

const _: () = assert!(core::mem::size_of::<Kevent>() == 32);

/// Filter: read readiness on an fd.
pub(crate) const EVFILT_READ: i16 = -1;
/// Filter: write readiness on an fd.
pub(crate) const EVFILT_WRITE: i16 = -2;
/// Filter: exceptional / out-of-band conditions on an fd.
pub(crate) const EVFILT_EXCEPT: i16 = -15;
/// Filter: user-triggered event — the cross-thread [`Waker`](crate::Waker)
/// backend. Darwin's value is `-10`.
pub(crate) const EVFILT_USER: i16 = -10;
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
/// Force a return path for the change list entry without delivering
/// any waiting events (used for batched change diagnostics).
pub(crate) const EV_RECEIPT: u16 = 0x0040;
/// Kernel signals EOF on a half-closed connection.
pub(crate) const EV_EOF: u16 = 0x8000;
/// Kernel signals an error inline in this event slot.
pub(crate) const EV_ERROR: u16 = 0x4000;

/// `timespec` for `kevent` timeouts. Same layout x86_64 + aarch64.
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

// ============================================================
// Batch datagram syscalls (`sendmsg_x` / `recvmsg_x`).
//
// XNU-private but ABI-stable (numbers verified against the
// macOS SDK `sys/syscall.h`; same calls the major QUIC
// stacks use on Darwin). Both REQUIRE a connected socket:
// the kernel rejects `msg_name` on this path, so the
// callers below never populate it.
// ============================================================

/// Send up to `datagrams.len()` datagrams on a CONNECTED
/// non-blocking UDP socket in one syscall. Returns how many
/// were accepted (can be short on send-buffer pressure;
/// `WouldBlock` when zero fit).
pub(crate) fn sendmsg_x(fd: RawFd, datagrams: &[&[u8]]) -> IoResult<usize> {
    // One iovec per message, stack-allocated up to the
    // batch cap the callers use.
    const CAP: usize = 64;
    let n = datagrams.len().min(CAP);
    let mut iovs = [Iovec {
        iov_base: core::ptr::null_mut(),
        iov_len: 0,
    }; CAP];
    let mut hdrs: [MsghdrX; CAP] = core::array::from_fn(|_| MsghdrX::zeroed());
    for (i, d) in datagrams[..n].iter().enumerate() {
        iovs[i] = Iovec {
            iov_base: d.as_ptr() as *mut u8,
            iov_len: d.len(),
        };
        hdrs[i].msg_iov = &mut iovs[i];
        hdrs[i].msg_iovlen = 1;
    }
    // SAFETY: `hdrs[..n]` points at live iovecs over live
    // borrows for the duration of the call; cnt = n.
    let ret = unsafe {
        syscall6(
            nr::SENDMSG_X,
            fd as usize,
            hdrs.as_mut_ptr() as usize,
            n,
            0,
            0,
            0,
        )
    };
    from_syscall_ret(ret).map(|v| v as usize)
}

/// Receive up to `bufs.len()` datagrams on a CONNECTED
/// non-blocking UDP socket in one syscall. Per-datagram
/// lengths land in `lens`; returns how many arrived
/// (`WouldBlock` when none queued).
pub(crate) fn recvmsg_x(fd: RawFd, bufs: &mut [&mut [u8]], lens: &mut [usize]) -> IoResult<usize> {
    const CAP: usize = 64;
    let n = bufs.len().min(lens.len()).min(CAP);
    let mut iovs = [Iovec {
        iov_base: core::ptr::null_mut(),
        iov_len: 0,
    }; CAP];
    let mut hdrs: [MsghdrX; CAP] = core::array::from_fn(|_| MsghdrX::zeroed());
    for (i, b) in bufs[..n].iter_mut().enumerate() {
        iovs[i] = Iovec {
            iov_base: b.as_mut_ptr(),
            iov_len: b.len(),
        };
        hdrs[i].msg_iov = &mut iovs[i];
        hdrs[i].msg_iovlen = 1;
    }
    // SAFETY: as above; the kernel writes each datagram's
    // byte count into `msg_datalen`.
    let ret = unsafe {
        syscall6(
            nr::RECVMSG_X,
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
        *l = hdrs[i].msg_datalen;
    }
    Ok(got)
}
