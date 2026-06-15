//! Linux `epoll` selector — readiness-based backend.

use super::raw;
use crate::types::{Interest, IoErrorKind, IoResult, Ready, Token};

/// `Selector` wraps the kernel epoll fd.
///
/// Created once by `Poll::new()`, shared through `Registry` clones.
/// Drop closes the underlying fd.
pub(crate) struct Selector {
    epfd: raw::RawFd,
}

impl Selector {
    pub(crate) fn new() -> IoResult<Self> {
        let epfd = raw::epoll_create1(raw::EPOLL_CLOEXEC)?;
        Ok(Self { epfd })
    }

    pub(crate) fn ctl_add(&self, fd: raw::RawFd, token: Token, interest: Interest) -> IoResult<()> {
        let event = raw::EpollEvent::new(interest_to_epoll_events(interest), token.0 as u64);
        raw::epoll_ctl(self.epfd, raw::EPOLL_CTL_ADD, fd, Some(&event))
    }

    pub(crate) fn ctl_mod(&self, fd: raw::RawFd, token: Token, interest: Interest) -> IoResult<()> {
        let event = raw::EpollEvent::new(interest_to_epoll_events(interest), token.0 as u64);
        raw::epoll_ctl(self.epfd, raw::EPOLL_CTL_MOD, fd, Some(&event))
    }

    pub(crate) fn ctl_del(&self, fd: raw::RawFd) -> IoResult<()> {
        raw::epoll_ctl(self.epfd, raw::EPOLL_CTL_DEL, fd, None)
    }

    /// Wait until at least one event is ready (or `timeout_ms`
    /// elapses). Retries on `EINTR` per the standard reactor
    /// convention; surface other errors to the caller.
    pub(crate) fn wait(&self, events: &mut [raw::EpollEvent], timeout_ms: i32) -> IoResult<usize> {
        loop {
            match raw::epoll_pwait(self.epfd, events, timeout_ms) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == IoErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        // Best-effort close. Drop cannot surface IO errors; the fd is
        // gone either way once the process exits.
        let _ = raw::close(self.epfd);
    }
}

/// Translate the public [`Interest`] bitset to epoll event flags.
fn interest_to_epoll_events(interest: Interest) -> u32 {
    let mut bits: u32 = 0;
    if interest.contains(Interest::READABLE) {
        // RDHUP gives us the public READ_CLOSED signal alongside
        // ordinary readability — register both as part of READABLE.
        bits |= raw::EPOLLIN | raw::EPOLLRDHUP;
    }
    if interest.contains(Interest::WRITABLE) {
        bits |= raw::EPOLLOUT;
    }
    if interest.contains(Interest::PRIORITY) {
        bits |= raw::EPOLLPRI;
    }
    // `Interest::AIO` has no `epoll` analogue; silently ignored.
    bits
}

/// Translate epoll event flags into the public [`Ready`] bitset.
pub(crate) fn epoll_events_to_ready(events: u32) -> Ready {
    let mut ready = Ready::empty();
    if events & raw::EPOLLIN != 0 {
        ready = ready.add(Ready::READABLE);
    }
    if events & raw::EPOLLOUT != 0 {
        ready = ready.add(Ready::WRITABLE);
    }
    if events & raw::EPOLLPRI != 0 {
        ready = ready.add(Ready::PRIORITY);
    }
    if events & raw::EPOLLRDHUP != 0 {
        ready = ready.add(Ready::READ_CLOSED);
    }
    if events & raw::EPOLLERR != 0 {
        ready = ready.add(Ready::ERROR);
    }
    if events & raw::EPOLLHUP != 0 {
        ready = ready.add(Ready::HANGUP);
    }
    ready
}

/// Read the `(events, data)` pair from a kernel `epoll_event`.
///
/// On x86_64 the kernel struct is `__attribute__((packed))` — the
/// `u64 data` field sits at byte offset 4 (not 8). On aarch64 the
/// struct uses natural alignment.
///
/// Going through [`core::ptr::addr_of`] + [`core::ptr::read_unaligned`]
/// avoids constructing a misaligned reference, which is UB on the
/// packed x86_64 layout even before any dereference.
pub(crate) fn read_event(ev: &raw::EpollEvent) -> (u32, u64) {
    let ev_ptr: *const raw::EpollEvent = ev;
    // SAFETY: `ev` is a valid `&EpollEvent` for the duration of this
    // call; `addr_of!` produces a raw pointer without creating a
    // reference; `read_unaligned` handles any alignment.
    unsafe {
        let events = core::ptr::read_unaligned(core::ptr::addr_of!((*ev_ptr).events));
        let data = core::ptr::read_unaligned(core::ptr::addr_of!((*ev_ptr).data));
        (events, data)
    }
}
