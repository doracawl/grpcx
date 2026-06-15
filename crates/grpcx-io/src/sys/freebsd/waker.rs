//! Cross-thread [`Waker`](crate::Waker) backend — kqueue `EVFILT_USER`.
//!
//! A user-triggerable kevent is registered under a fixed `ident`.
//! [`wake`](Waker::wake) fires it with `NOTE_TRIGGER` from any thread
//! (`kevent` on a shared kq is thread-safe). `EV_CLEAR` auto-resets the
//! event after delivery, so [`reset`](Waker::reset) is a no-op.

use crate::reactor::Registry;
use crate::types::{IoResult, Token};

pub(crate) struct Waker {
    registry: Registry,
    ident: usize,
    token: Token,
}

impl Waker {
    pub(crate) fn new(registry: &Registry, token: Token) -> IoResult<Self> {
        let ident = token.0;
        registry.register_user(ident, token)?;
        Ok(Self {
            registry: registry.clone(),
            ident,
            token,
        })
    }

    pub(crate) fn wake(&self) -> IoResult<()> {
        self.registry.trigger_user(self.ident, self.token)
    }

    pub(crate) fn reset(&self) -> IoResult<()> {
        // `EV_CLEAR` auto-resets the user event; nothing to drain.
        Ok(())
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        let _ = self.registry.deregister_user(self.ident);
    }
}
