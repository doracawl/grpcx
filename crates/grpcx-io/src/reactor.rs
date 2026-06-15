//! [`Poll`] — the readiness reactor — and its cloneable [`Registry`]
//! handle.

use crate::event::Events;
use crate::source::Source;
use crate::types::{Interest, IoError, IoErrorKind, IoResult, Token};
use core::time::Duration;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
use alloc::sync::Arc;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
type SelectorRef = Arc<crate::sys::Selector>;

/// The polling driver — owns OS-level reactor state.
///
/// Construct one per worker thread (typical) and reuse across
/// [`Self::poll`] calls. To let other threads register sources,
/// clone the [`Registry`] obtained from [`Self::registry`].
///
/// # Backend status
///
/// Linux (epoll, optional io_uring), macOS / iOS (kqueue), and
/// FreeBSD (kqueue) are wired. Windows / other targets compile but
/// every reactor call returns [`IoErrorKind::BackendUnavailable`].
pub struct Poll {
    registry: Registry,
}

/// Lightweight, cloneable handle for source registration.
///
/// `Registry` carries the minimal state the reactor needs for
/// [`Self::register`] / [`Self::reregister`] / [`Self::deregister`].
/// Cloning is cheap (`Arc` bump on backends that need one).
#[derive(Clone)]
pub struct Registry {
    /// Shared reference to the platform selector. `None` only on
    /// targets that have no backend yet (e.g. Windows).
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    inner: Option<SelectorRef>,
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    _private: (),
}

impl Poll {
    /// Construct a new reactor driver bound to this OS thread.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub fn new() -> IoResult<Self> {
        let selector = crate::sys::Selector::new()?;
        Ok(Self {
            registry: Registry {
                inner: Some(Arc::new(selector)),
            },
        })
    }

    /// Construct a new reactor driver bound to this OS thread.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    pub fn new() -> IoResult<Self> {
        Err(IoError::new(IoErrorKind::BackendUnavailable, 0))
    }

    /// Borrow this reactor's registration handle.
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Wait for one or more sources to become ready, draining ready
    /// events into `events`.
    ///
    /// `timeout = None` blocks indefinitely; `Some(d)` waits at most
    /// `d` and may return with `events` empty if `d` elapses with
    /// no readiness.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub fn poll(&mut self, events: &mut Events, timeout: Option<Duration>) -> IoResult<()> {
        let Some(selector) = self.registry.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        events.poll_with(selector.as_ref(), timeout)
    }

    /// Wait for one or more sources to become ready, draining ready
    /// events into `events`.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    pub fn poll(&mut self, _events: &mut Events, _timeout: Option<Duration>) -> IoResult<()> {
        Err(IoError::new(IoErrorKind::BackendUnavailable, 0))
    }
}

impl Registry {
    /// Register `source` under `token`, requesting notifications for
    /// `interest`.
    ///
    /// Generic over `S` so the dispatch monomorphizes; pass any
    /// concrete `Source` impl or `&mut dyn Source`.
    pub fn register<S>(&self, source: &mut S, token: Token, interest: Interest) -> IoResult<()>
    where
        S: Source + ?Sized,
    {
        source.register(self, token, interest)
    }

    /// Update an already-registered source's token and interest set.
    pub fn reregister<S>(&self, source: &mut S, token: Token, interest: Interest) -> IoResult<()>
    where
        S: Source + ?Sized,
    {
        source.reregister(self, token, interest)
    }

    /// Remove `source` from this reactor.
    pub fn deregister<S>(&self, source: &mut S) -> IoResult<()>
    where
        S: Source + ?Sized,
    {
        source.deregister(self)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub(crate) fn ctl_add(
        &self,
        fd: crate::sys::RawFd,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.ctl_add(fd, token, interest)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub(crate) fn ctl_mod(
        &self,
        fd: crate::sys::RawFd,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.ctl_mod(fd, token, interest)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub(crate) fn ctl_del(&self, fd: crate::sys::RawFd) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.ctl_del(fd)
    }

    // EVFILT_USER backend for the kqueue `Waker` (the Linux `Waker`
    // uses an eventfd registered through `ctl_add` instead).
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    pub(crate) fn register_user(&self, ident: usize, token: Token) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.register_user(ident, token)
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    pub(crate) fn trigger_user(&self, ident: usize, token: Token) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.trigger_user(ident, token.0)
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    pub(crate) fn deregister_user(&self, ident: usize) -> IoResult<()> {
        let Some(selector) = self.inner.as_ref() else {
            return Err(IoError::new(IoErrorKind::BackendUnavailable, 0));
        };
        selector.deregister_user(ident)
    }
}
