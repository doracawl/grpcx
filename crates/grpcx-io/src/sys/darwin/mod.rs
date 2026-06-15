//! Darwin (macOS / iOS) backend: kqueue readiness reactor on raw
//! syscalls. T24 lands the inline-asm syscall layer; T25 adds the
//! kevent ABI; T26+ wire it into `Poll` / `Registry`.

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

/// Batch-datagram syscalls (`sendmsg_x` / `recvmsg_x`) for
/// CONNECTED non-blocking UDP sockets — the per-MTU-syscall
/// fix for bulk transfers (one syscall moves up to 64
/// datagrams). XNU-private but ABI-stable; numbers verified
/// against the macOS SDK.
pub mod udp_batch {
    use super::raw;
    use crate::IoResult;

    /// Send up to 64 datagrams in one syscall on a CONNECTED
    /// non-blocking UDP socket. Returns how many were
    /// accepted (short on send-buffer pressure; `WouldBlock`
    /// when zero fit).
    pub fn send_batch(fd: raw::RawFd, datagrams: &[&[u8]]) -> IoResult<usize> {
        raw::sendmsg_x(fd, datagrams)
    }

    /// Receive up to 64 datagrams in one syscall on a
    /// CONNECTED non-blocking UDP socket. Per-datagram byte
    /// counts land in `lens`; returns how many arrived
    /// (`WouldBlock` when none queued).
    pub fn recv_batch(
        fd: raw::RawFd,
        bufs: &mut [&mut [u8]],
        lens: &mut [usize],
    ) -> IoResult<usize> {
        raw::recvmsg_x(fd, bufs, lens)
    }
}
pub(crate) use waker::Waker;

/// Helpers reachable from external `benches/` (and only from there).
///
/// Wrapped over `pub(crate)` syscalls so the crate's public surface
/// stays narrow. `#[doc(hidden)]` keeps them out of rendered docs.
#[doc(hidden)]
pub mod __bench_helpers {
    use super::raw;
    use crate::IoResult;

    /// Raw file descriptor.
    pub type RawFd = raw::RawFd;

    /// Compatibility constant for benches written against the Linux
    /// `__bench_helpers::pipe2(O_CLOEXEC)` shape. Darwin has no
    /// `pipe2` — [`pipe2`] ignores the flag and applies
    /// `FD_CLOEXEC` unconditionally via `fcntl`.
    pub const O_CLOEXEC: i32 = raw::FD_CLOEXEC;

    /// `pipe2(flags)` — Linux signature. On Darwin the `flags`
    /// argument is ignored (Darwin's `pipe()` always pairs with an
    /// explicit `fcntl` setting `FD_CLOEXEC`, which
    /// `raw::pipe_cloexec` already does). Returns
    /// `(read_end, write_end)`.
    pub fn pipe2(_flags: i32) -> IoResult<(RawFd, RawFd)> {
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
