//! Cross-OS wake-fd primitive.
//!
//! v1 uses `pipe(2)` everywhere — write one byte to
//! wake; read drains. Linux can upgrade to a true
//! `eventfd(2)` in a perf push without changing the
//! public surface here.
//!
//! Wake fds are exchanged between rendezvous peers via
//! `SCM_RIGHTS` ancillary data.

use core::ffi::c_int;
use std::os::fd::{FromRawFd, OwnedFd};

use crate::RpcError;

unsafe extern "C" {
    fn pipe(fildes: *mut c_int) -> c_int;
}

/// One end of a directional wake channel.
///
/// `read_fd` is held by the side that *waits* (consumer);
/// `write_fd` is held by the side that *signals*
/// (producer). For a bidirectional ring you need two
/// `WakePair`s — one per direction.
pub(crate) struct WakePair {
    pub read_fd: OwnedFd,
    pub write_fd: OwnedFd,
}

impl WakePair {
    /// Create a new wake-fd pair via `pipe(2)`.
    pub(crate) fn create() -> Result<Self, RpcError> {
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
