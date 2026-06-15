//! Linux backend: epoll readiness reactor + io_uring completion ring
//! on raw syscalls. The two backends ship side-by-side as siblings,
//! not as a single abstraction — see `io_uring` for rationale.

mod epoll;
mod io_uring;
mod raw;
mod source_fd;
mod waker;

#[cfg(test)]
mod tests;

pub(crate) use epoll::{Selector, epoll_events_to_ready, read_event};
pub use io_uring::{Cqe, Ring, Sqe};
pub(crate) use raw::EpollEvent;
pub use raw::RawFd;
pub use source_fd::SourceFd;
pub(crate) use waker::Waker;

/// Batch-datagram syscalls (`sendmmsg` / `recvmmsg`) for
/// CONNECTED non-blocking UDP sockets — the per-MTU-syscall
/// fix for bulk transfers (one syscall moves up to 64
/// datagrams). Same contract as the Darwin
/// `sendmsg_x`/`recvmsg_x` pair.
pub mod udp_batch {
    use super::raw;
    use crate::IoResult;

    /// Send up to 64 datagrams in one syscall on a CONNECTED
    /// non-blocking UDP socket. Returns how many were
    /// accepted (short on send-buffer pressure; `WouldBlock`
    /// when zero fit).
    pub fn send_batch(fd: raw::RawFd, datagrams: &[&[u8]]) -> IoResult<usize> {
        raw::sendmmsg(fd, datagrams)
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
        raw::recvmmsg(fd, bufs, lens)
    }
}

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
    /// `O_CLOEXEC` open-flag value.
    pub const O_CLOEXEC: i32 = raw::O_CLOEXEC;

    /// `pipe2(flags)` — returns `(read_end, write_end)`.
    pub fn pipe2(flags: i32) -> IoResult<(RawFd, RawFd)> {
        raw::pipe2(flags)
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
