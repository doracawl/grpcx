//! Peer-credential fetch for AF_UNIX sockets.
//!
//! Zero external dependencies: the syscalls are declared
//! via `unsafe extern "C"` blocks. The two backends
//! (Linux `SO_PEERCRED`, Darwin / FreeBSD
//! `getpeereid(3)`) map to the same [`PeerCred`] shape.
//!
//! `SO_PEERCRED` is `17` on every modern Linux
//! architecture the kernel uapi headers reach
//! (`asm-generic/socket.h`). The historical outliers
//! (sparc / parisc / alpha) are not targets we support.

use std::os::unix::io::RawFd;

use crate::PeerCred;

/// Retrieve peer credentials for a connected AF_UNIX
/// socket. Returns `None` if the syscall fails (e.g.
/// the socket has been closed by the peer between
/// `accept` and the credential fetch).
#[inline]
pub fn fetch(fd: RawFd) -> Option<PeerCred> {
    plat::fetch(fd)
}

// ============================================================
// Linux / Android — SO_PEERCRED on a connected AF_UNIX.
// ============================================================

#[cfg(any(target_os = "linux", target_os = "android"))]
mod plat {
    use core::ffi::c_void;

    use super::{PeerCred, RawFd};

    const SOL_SOCKET: i32 = 1;
    // SO_PEERCRED is 17 on every modern Linux
    // architecture's <asm-generic/socket.h>. Old
    // outliers (sparc / parisc / alpha) used different
    // values; they are not targets we support.
    const SO_PEERCRED: i32 = 17;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }

    unsafe extern "C" {
        fn getsockopt(
            sockfd: i32,
            level: i32,
            optname: i32,
            optval: *mut c_void,
            optlen: *mut u32,
        ) -> i32;
    }

    pub(super) fn fetch(fd: RawFd) -> Option<PeerCred> {
        let mut cred = Ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut sz = core::mem::size_of::<Ucred>() as u32;
        // SAFETY: `cred` is a properly aligned and sized
        // `Ucred` on the caller's stack; we pass a
        // pointer to it as `*mut c_void` (kernel expects
        // any byte pointer here) and the matching length.
        // `sz` is `&mut` so the kernel may shrink it; we
        // ignore the written-back length because the only
        // accepted value is `sizeof(Ucred)` (kernel
        // returns EINVAL otherwise, surfaced as `r != 0`).
        let r = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                SO_PEERCRED,
                core::ptr::from_mut(&mut cred).cast::<c_void>(),
                &mut sz,
            )
        };
        if r != 0 {
            return None;
        }
        Some(PeerCred {
            uid: cred.uid,
            gid: Some(cred.gid),
            pid: Some(cred.pid),
        })
    }
}

// ============================================================
// Darwin / iOS / FreeBSD — getpeereid(3).
// ============================================================

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
mod plat {
    use super::{PeerCred, RawFd};

    unsafe extern "C" {
        fn getpeereid(s: i32, euid: *mut u32, egid: *mut u32) -> i32;
    }

    pub(super) fn fetch(fd: RawFd) -> Option<PeerCred> {
        let mut uid: u32 = 0;
        let mut gid: u32 = 0;
        // SAFETY: both pointers are to local `u32`s
        // allocated on the caller's stack; the libc /
        // libsystem implementation of `getpeereid` writes
        // exactly one `uid_t` (= u32) into each.
        let r = unsafe { getpeereid(fd, &mut uid, &mut gid) };
        if r != 0 {
            return None;
        }
        Some(PeerCred {
            uid,
            gid: Some(gid),
            pid: None, // not exposed by getpeereid(3)
        })
    }
}

// ============================================================
// Other unix-like — credentials not implemented.
// ============================================================

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
mod plat {
    use super::{PeerCred, RawFd};

    pub(super) fn fetch(_fd: RawFd) -> Option<PeerCred> {
        None
    }
}
