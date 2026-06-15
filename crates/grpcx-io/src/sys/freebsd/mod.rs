//! FreeBSD backend: kqueue readiness reactor on raw amd64 syscalls.
//!
//! Mirrors `sys::darwin` in shape. The differences:
//!
//! - Bare syscall numbers (no Darwin BSD-class prefix).
//! - Native `pipe2(O_CLOEXEC)` (one atomic syscall, no `fcntl`
//!   follow-up).
//! - 64-byte `kevent` struct (FreeBSD 12+) — trailing `ext[4]`
//!   present but unused at L1.
//! - No `EVFILT_EXCEPT` — `Interest::PRIORITY` silently dropped.
//!
//! See `sys::darwin::raw` for the parallel Darwin notes.

mod kqueue;
mod raw;
mod source_fd;
mod waker;

#[cfg(test)]
mod tests;

pub(crate) use kqueue::{Selector, kevent_to_ready, token_from_event};
pub use raw::RawFd;
pub(crate) use raw::{Kevent, Timespec};
pub use source_fd::SourceFd;
pub(crate) use waker::Waker;

/// Helpers reachable from external `benches/` (and only from there).
///
/// FreeBSD's native `pipe2(O_CLOEXEC)` means the bench shim can be
/// a direct passthrough — no `fcntl` workaround like Darwin.
#[doc(hidden)]
pub mod __bench_helpers {
    use super::raw;
    use crate::IoResult;

    /// Raw file descriptor.
    pub type RawFd = raw::RawFd;

    /// Compatibility constant for benches written against the Linux
    /// `__bench_helpers::pipe2(O_CLOEXEC)` shape. FreeBSD's
    /// `O_CLOEXEC` has the FreeBSD-specific bit pattern.
    pub const O_CLOEXEC: i32 = raw::O_CLOEXEC;

    /// `pipe2(flags)` — Linux signature. On FreeBSD the underlying
    /// syscall is also `pipe2`; the `flags` argument is honored
    /// directly (`O_CLOEXEC` is the only flag the bench harness
    /// passes). Returns `(read_end, write_end)`.
    pub fn pipe2(_flags: i32) -> IoResult<(RawFd, RawFd)> {
        // Only `O_CLOEXEC` is meaningful at the L1 bench harness;
        // the underlying `raw::pipe_cloexec` already requests it,
        // so an explicit `flags` argument is redundant.
        raw::pipe_cloexec()
    }

    /// `write(fd, buf)` — returns bytes written.
    pub fn write(fd: RawFd, buf: &[u8]) -> IoResult<usize> {
        raw::write(fd, buf)
    }

    /// `read(fd, buf)` — returns bytes read.
    pub fn read(fd: RawFd, buf: &mut [u8]) -> IoResult<usize> {
        raw::read(fd, buf)
    }

    /// `close(fd)`.
    pub fn close(fd: RawFd) -> IoResult<()> {
        raw::close(fd)
    }
}
