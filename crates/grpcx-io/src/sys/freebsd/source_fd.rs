//! `SourceFd` — wrap a raw FreeBSD fd so it implements [`Source`].
//!
//! Mirror of the Darwin module's [`SourceFd`]; same trait, same
//! semantics. The two are split-by-cfg only because each backend
//! owns its own `RawFd` alias.
//!
//! [`SourceFd`]: crate::sys::darwin::SourceFd

use super::raw::RawFd;
use crate::reactor::Registry;
use crate::source::Source;
use crate::types::{Interest, IoResult, Token};

/// Wrap a raw FreeBSD file descriptor as a registerable [`Source`].
///
/// `SourceFd` does **not** take ownership of the fd; the caller
/// retains responsibility for closing it. Per the contract on
/// [`Source`], the fd must remain valid between
/// [`Source::register`] and [`Source::deregister`] / [`Poll`] drop.
///
/// # Closing order
///
/// `deregister(fd)` (kqueue `EV_DELETE`) followed by `close(fd)`
/// is the safe order. Closing first is allowed by kqueue (the
/// kernel auto-evicts on `close`), but the explicit deregister
/// keeps the bookkeeping symmetric with epoll's stricter rule.
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
