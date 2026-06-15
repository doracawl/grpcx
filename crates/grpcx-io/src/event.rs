//! Events container returned by [`Poll::poll`](crate::Poll::poll).

use crate::types::{Ready, Token};
use alloc::vec::Vec;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
use core::time::Duration;

/// A single readiness observation from the reactor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    token: Token,
    ready: Ready,
}

// Static size anchor: `Token` is `usize` (8 B on 64-bit), `Ready` is
// `u16` (2 B), structure aligned to `usize` → 16 B on 64-bit targets.
// Catches accidental field bloat.
const _: () = assert!(core::mem::size_of::<Event>() <= 2 * core::mem::size_of::<usize>());

impl Event {
    /// Construct a new event.
    ///
    /// Backends call this when translating native events into the
    /// portable shape. Also useful for hand-rolled
    /// [`Source`](crate::Source) impls that surface readiness from
    /// non-OS sources (e.g. inter-thread wakers).
    pub const fn new(token: Token, ready: Ready) -> Self {
        Self { token, ready }
    }

    /// The token the source was registered with.
    pub const fn token(&self) -> Token {
        self.token
    }

    /// The readiness bits the backend observed.
    pub const fn ready(&self) -> Ready {
        self.ready
    }
}

/// Container the reactor drains ready events into.
///
/// Pre-allocate with [`Self::with_capacity`]; the same `Events` is
/// reused across many [`Poll::poll`](crate::Poll::poll) calls
/// without re-allocating.
pub struct Events {
    /// Kernel-shaped event buffer; filled by `epoll_pwait`.
    #[cfg(target_os = "linux")]
    raw: Vec<crate::sys::EpollEvent>,
    /// Kernel-shaped event buffer; filled by `kevent`.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    raw: Vec<crate::sys::Kevent>,
    /// Translated events; iterated by the user.
    translated: Vec<Event>,
    /// Originally requested capacity; the kernel reads this many
    /// slots from `raw` per call.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    capacity: usize,
}

impl Events {
    /// Construct an `Events` with capacity for `cap` events per
    /// drain.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd"
            ))]
            raw: Vec::with_capacity(cap),
            translated: Vec::with_capacity(cap),
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd"
            ))]
            capacity: cap,
        }
    }

    /// Number of events drained on the most recent
    /// [`Poll::poll`](crate::Poll::poll).
    pub fn len(&self) -> usize {
        self.translated.len()
    }

    /// True iff no events were drained.
    pub fn is_empty(&self) -> bool {
        self.translated.is_empty()
    }

    /// Iterate the events drained on the most recent poll.
    pub fn iter(&self) -> EventsIter<'_> {
        EventsIter {
            inner: self.translated.iter(),
        }
    }

    /// Drive one `Selector::wait` cycle, translating kernel events
    /// into the portable shape. Called by [`Poll::poll`].
    ///
    /// `self.raw` is grown to `self.capacity` on first call and
    /// stays there — the kernel writes into slots 0..n on every
    /// call and we only iterate that prefix, so stale data in
    /// n..capacity is harmless. v0.7 and earlier did
    /// `clear + resize` every poll, paying a ~capacity ×
    /// `size_of::<EpollEvent>()` memset on the hot path.
    ///
    /// [`Poll::poll`]: crate::Poll::poll
    #[cfg(target_os = "linux")]
    pub(crate) fn poll_with(
        &mut self,
        selector: &crate::sys::Selector,
        timeout: Option<Duration>,
    ) -> crate::types::IoResult<()> {
        self.translated.clear();
        if self.raw.len() < self.capacity {
            self.raw
                .resize(self.capacity, crate::sys::EpollEvent::default());
        }

        let timeout_ms = duration_to_ms(timeout);
        let n = selector.wait(&mut self.raw[..], timeout_ms)?;

        for ev in &self.raw[..n] {
            let (events, data) = crate::sys::read_event(ev);
            let ready = crate::sys::epoll_events_to_ready(events);
            let token = Token(data as usize);
            self.translated.push(Event::new(token, ready));
        }

        Ok(())
    }

    /// kqueue-family variant — `kevent` instead of `epoll_pwait`.
    /// Same portable contract: drain ready events into
    /// `self.translated` for the caller to iterate. Used by Darwin
    /// (macOS / iOS) and FreeBSD.
    ///
    /// Lazy `raw` fill — see the Linux variant's doc for the
    /// "kernel only writes 0..n, stale tail is safe" argument.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    pub(crate) fn poll_with(
        &mut self,
        selector: &crate::sys::Selector,
        timeout: Option<Duration>,
    ) -> crate::types::IoResult<()> {
        self.translated.clear();
        if self.raw.len() < self.capacity {
            self.raw
                .resize(self.capacity, crate::sys::Kevent::default());
        }

        let timeout_spec = duration_to_timespec(timeout);
        let n = selector.wait(&mut self.raw[..], timeout_spec)?;

        for ev in &self.raw[..n] {
            let ready = crate::sys::kevent_to_ready(ev);
            let token = crate::sys::token_from_event(ev);
            self.translated.push(Event::new(token, ready));
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn duration_to_ms(d: Option<Duration>) -> i32 {
    match d {
        None => -1,
        Some(d) => {
            let ms = d.as_millis();
            if ms > i32::MAX as u128 {
                i32::MAX
            } else {
                ms as i32
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn duration_to_timespec(d: Option<Duration>) -> Option<crate::sys::Timespec> {
    // `None` → kevent blocks indefinitely. `Some(d)` → seconds +
    // nanoseconds split.
    d.map(|dur| crate::sys::Timespec {
        tv_sec: dur.as_secs() as isize,
        tv_nsec: dur.subsec_nanos() as isize,
    })
}

impl<'a> IntoIterator for &'a Events {
    type Item = &'a Event;
    type IntoIter = EventsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over events drained from a reactor poll.
pub struct EventsIter<'a> {
    inner: core::slice::Iter<'a, Event>,
}

impl<'a> Iterator for EventsIter<'a> {
    type Item = &'a Event;

    fn next(&mut self) -> Option<&'a Event> {
        self.inner.next()
    }
}
