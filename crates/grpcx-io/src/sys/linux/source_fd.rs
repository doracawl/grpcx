//! `SourceFd` — wrap a raw Linux fd so it implements [`Source`].

use super::raw::RawFd;
use crate::reactor::Registry;
use crate::source::Source;
use crate::types::{Interest, IoResult, Token};

/// Wrap a raw Linux file descriptor as a registerable [`Source`].
///
/// `SourceFd` does **not** take ownership of the fd; the caller
/// retains responsibility for closing it (via the OS-level
/// equivalent of `close(2)`). Per the contract on [`Source`], the
/// fd must remain valid between [`Source::register`] and
/// [`Source::deregister`] / [`Poll`] drop.
///
/// # Closing order
///
/// `EPOLL_CTL_DEL` followed by `close(fd)` is the safe order.
/// Closing the fd while still registered leaks kernel state on
/// older kernels (< 4.x); even on current kernels it complicates
/// recycling because `epoll` keeps the closed fd on its interest
/// list until the next event flush.
///
/// [`Poll`]: crate::Poll
pub struct SourceFd {
    /// The wrapped file descriptor.
    pub fd: RawFd,
}

impl SourceFd {
    /// Construct a new `SourceFd` from a raw file descriptor.
    pub const fn new(fd: RawFd) -> Self {
        Self { fd }
    }
}

impl Source for SourceFd {
    fn register(&mut self, registry: &Registry, token: Token, interest: Interest) -> IoResult<()> {
        registry.ctl_add(self.fd, token, interest)
    }

    fn reregister(
        &mut self,
        registry: &Registry,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        registry.ctl_mod(self.fd, token, interest)
    }

    fn deregister(&mut self, registry: &Registry) -> IoResult<()> {
        registry.ctl_del(self.fd)
    }
}
