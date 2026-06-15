//! Cross-thread [`Waker`](crate::Waker) backend — Linux `eventfd`.
//!
//! An `eventfd` (created `EFD_NONBLOCK`) is registered with the epoll
//! reactor as a readable source. [`wake`](Waker::wake) writes a `u64`
//! to make it readable from any thread; because epoll is
//! level-triggered, [`reset`](Waker::reset) drains the counter after a
//! fire so it does not re-report on the next `poll`.

use super::raw;
use crate::reactor::Registry;
use crate::types::{Interest, IoErrorKind, IoResult, Token};

pub(crate) struct Waker {
    fd: raw::RawFd,
    registry: Registry,
}

impl Waker {
    pub(crate) fn new(registry: &Registry, token: Token) -> IoResult<Self> {
        let fd = raw::eventfd(0, raw::EFD_CLOEXEC | raw::EFD_NONBLOCK)?;
        if let Err(e) = registry.ctl_add(fd, token, Interest::READABLE) {
            let _ = raw::close(fd);
            return Err(e);
        }
        Ok(Self {
            fd,
            registry: registry.clone(),
        })
    }

    pub(crate) fn wake(&self) -> IoResult<()> {
        let buf = 1u64.to_ne_bytes();
        match raw::write(self.fd, &buf) {
            Ok(_) => Ok(()),
            // Counter saturated (near u64::MAX) — a wakeup is already
            // pending, so dropping this one is harmless.
            Err(e) if e.kind() == IoErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn reset(&self) -> IoResult<()> {
        let mut buf = [0u8; 8];
        match raw::read(self.fd, &mut buf) {
            Ok(_) => Ok(()),
            // Already drained.
            Err(e) if e.kind() == IoErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        let _ = self.registry.ctl_del(self.fd);
        let _ = raw::close(self.fd);
    }
}
