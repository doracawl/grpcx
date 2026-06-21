//! Cross-OS wake-fd primitive.
//!
//! On Linux uses `eventfd(2)` — single fd, 8-byte
//! counter, no buffer-fill backpressure, ~30-40 %
//! cheaper syscall than `pipe(2)` on kernel 6.x. The
//! same fd is both read and write end; we hand a
//! cloned copy to the peer so each side has an
//! independent `OwnedFd`.
//!
//! On all other Unixes uses `pipe(2)` — two fds (read
//! end + write end), 1-byte writes, 1-byte reads.
//!
//! The public `WakePair` shape is identical on both
//! platforms; the helpers [`wake_signal`] / [`wake_wait`]
//! hide the per-platform byte width (8 B on Linux,
//! 1 B on Darwin / FreeBSD / NetBSD).
//!
//! Wake fds are exchanged between rendezvous peers via
//! `SCM_RIGHTS` ancillary data.

use core::ffi::c_int;
use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};

use crate::RpcError;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn eventfd(initval: u32, flags: c_int) -> c_int;
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" {
    fn pipe(fildes: *mut c_int) -> c_int;
}

/// One end of a directional wake channel.
///
/// `read_fd` is held by the side that *waits* (consumer);
/// `write_fd` is held by the side that *signals*
/// (producer). For a bidirectional ring you need two
/// `WakePair`s — one per direction.
///
/// On Linux both fds point at the same underlying
/// `eventfd`. On other Unixes they are the two ends of
/// a `pipe(2)`.
pub(crate) struct WakePair {
    pub read_fd: OwnedFd,
    pub write_fd: OwnedFd,
}

impl WakePair {
    /// Create a new wake-fd pair. Linux uses `eventfd(2)`
    /// + `try_clone`; other Unixes use `pipe(2)`.
    pub(crate) fn create() -> Result<Self, RpcError> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: eventfd is a regular Linux syscall;
            // no aliasing concerns.
            let r = unsafe { eventfd(0, 0) };
            if r < 0 {
                return Err(RpcError::Transport("shmem: eventfd(2) failed"));
            }
            // SAFETY: fd just returned from the kernel,
            // we own it until passed via OwnedFd.
            let primary = unsafe { OwnedFd::from_raw_fd(r) };
            // Clone for the second handle. Both fds refer
            // to the same eventfd; SCM_RIGHTS over the
            // rendezvous socket lets one side send each
            // independently.
            let secondary = primary
                .try_clone()
                .map_err(|_| RpcError::Transport("shmem: eventfd dup failed"))?;
            Ok(WakePair {
                read_fd: primary,
                write_fd: secondary,
            })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let mut fds = [0i32; 2];
            // SAFETY: fds buffer is two ints on this stack
            // frame; `pipe` writes exactly two ints.
            let r = unsafe { pipe(fds.as_mut_ptr()) };
            if r != 0 {
                return Err(RpcError::Transport("shmem: pipe(2) failed"));
            }
            // SAFETY: each fd just returned from the kernel
            // and is owned by us until handed off via
            // `OwnedFd`.
            let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
            let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            Ok(WakePair { read_fd, write_fd })
        }
    }
}

/// Send one notification on the wake fd.
///
/// On Linux this is an 8-byte `1u64` counter increment
/// on the underlying `eventfd`. On other Unixes it is a
/// 1-byte write to the pipe.
#[inline]
pub(crate) fn wake_signal(fd: &mut File) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(target_os = "linux")]
    {
        // eventfd write: add the supplied 8-byte u64 to
        // the internal counter. Value 1 suffices — any
        // non-zero value flips the fd to "readable".
        let buf: [u8; 8] = 1u64.to_le_bytes();
        fd.write_all(&buf)
    }
    #[cfg(not(target_os = "linux"))]
    {
        fd.write_all(&[1u8])
    }
}

/// Block until at least one notification arrives, then
/// drain it.
///
/// On Linux this is an 8-byte counter read on the
/// `eventfd`; the kernel zeroes the counter on read
/// (default mode, no `EFD_SEMAPHORE`). On other Unixes
/// this is a single-byte pipe read.
#[inline]
pub(crate) fn wake_wait(fd: &mut File) -> std::io::Result<()> {
    use std::io::Read;
    #[cfg(target_os = "linux")]
    {
        let mut buf = [0u8; 8];
        fd.read_exact(&mut buf)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut buf = [0u8; 1];
        fd.read_exact(&mut buf)
    }
}
