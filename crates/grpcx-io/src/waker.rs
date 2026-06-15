//! [`Waker`] — cross-thread reactor wakeup (the `mio::Waker` analogue).

use crate::reactor::Registry;
use crate::types::{IoResult, Token};

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
use crate::types::{IoError, IoErrorKind};

/// A handle that wakes a blocked [`Poll::poll`](crate::Poll::poll) from
/// any thread.
///
/// Bound to a [`Registry`] and a [`Token`] at construction. Calling
/// [`wake`](Self::wake) — safe from any thread — makes the associated
/// reactor's `poll` return an event tagged with that token. This is the
/// primitive a runtime uses to unpark its IO driver when a task is woken
/// from a different thread (a cross-thread channel send, a work-stealing
/// peer, etc.).
///
/// After the poller observes the waker's token it must call
/// [`reset`](Self::reset) to re-arm. On `kqueue` this is a no-op (the
/// `EVFILT_USER` event auto-resets); on Linux it drains the backing
/// `eventfd` so the level-triggered epoll does not re-report it.
///
/// # Backends
///
/// - Linux: `eventfd`.
/// - macOS / iOS / FreeBSD: `kqueue` `EVFILT_USER`.
/// - Other targets: every method returns
///   [`BackendUnavailable`](crate::IoErrorKind::BackendUnavailable).
pub struct Waker {
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    inner: crate::sys::Waker,
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    _private: (),
}

impl Waker {
    /// Create a waker bound to `registry`, delivering on `token`.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub fn new(registry: &Registry, token: Token) -> IoResult<Self> {
        Ok(Self {
            inner: crate::sys::Waker::new(registry, token)?,
        })
    }

    /// Create a waker bound to `registry`, delivering on `token`.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    pub fn new(_registry: &Registry, _token: Token) -> IoResult<Self> {
        Err(IoError::new(IoErrorKind::BackendUnavailable, 0))
    }

    /// Wake the bound reactor. Safe to call from any thread.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub fn wake(&self) -> IoResult<()> {
        self.inner.wake()
    }

    /// Wake the bound reactor. Safe to call from any thread.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    pub fn wake(&self) -> IoResult<()> {
        Err(IoError::new(IoErrorKind::BackendUnavailable, 0))
    }

    /// Re-arm after the waker's token has been observed in `poll`.
    /// No-op on `kqueue`; drains the `eventfd` on Linux.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    pub fn reset(&self) -> IoResult<()> {
        self.inner.reset()
    }

    /// Re-arm after the waker's token has been observed in `poll`.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    pub fn reset(&self) -> IoResult<()> {
        Err(IoError::new(IoErrorKind::BackendUnavailable, 0))
    }
}
