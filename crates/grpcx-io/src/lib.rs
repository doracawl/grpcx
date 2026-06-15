#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![no_std]

//! Internal layout: the public surface lives across `types`,
//! `source`, `reactor`, and `event` modules; `sys` contains the
//! per-platform backend (Linux `epoll` as of v0.2).

extern crate alloc;

mod event;
mod reactor;
mod source;
mod sys;
mod types;
mod waker;

pub use event::{Event, Events, EventsIter};
pub use reactor::{Poll, Registry};
pub use source::Source;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
#[doc(hidden)]
pub use sys::__bench_helpers;
pub use waker::Waker;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
#[doc(hidden)]
pub mod __fuzz_helpers {
    //! Decoder helpers exposed for `libfuzzer-sys` fuzz targets only.
    //!
    //! These reach into `pub(crate)` translation paths
    //! (`epoll_events_to_ready`, `kevent_to_ready`, ...) so
    //! `fuzz/fuzz_targets/*.rs` can drive the per-OS event decoder
    //! with arbitrary bytes. Not part of the stable API; gated to
    //! `#[doc(hidden)]` and only reachable through this module path.

    use crate::Event;
    #[cfg(target_os = "linux")]
    use crate::types::Token;

    /// Decode a single event from a kernel-shaped byte slice. Returns
    /// `None` if the slice is too short. Backend-specific:
    /// - Linux: reads one `EpollEvent`.
    /// - Darwin / iOS: reads one 32-byte `Kevent`.
    /// - FreeBSD: reads one 64-byte `Kevent`.
    #[cfg(target_os = "linux")]
    pub fn decode_one_event(bytes: &[u8]) -> Option<Event> {
        const N: usize = core::mem::size_of::<crate::sys::EpollEvent>();
        if bytes.len() < N {
            return None;
        }
        let mut buf = [0u8; N];
        buf.copy_from_slice(&bytes[..N]);
        // SAFETY: `buf` is a local owned [u8; N]; read_unaligned
        // is the only sound way to read a `repr(packed)` struct
        // from a possibly-misaligned source.
        let ev: crate::sys::EpollEvent = unsafe { core::ptr::read_unaligned(buf.as_ptr().cast()) };
        let (events, data) = crate::sys::read_event(&ev);
        let ready = crate::sys::epoll_events_to_ready(events);
        Some(Event::new(Token(data as usize), ready))
    }

    /// Decode a single event from a kernel-shaped byte slice (kqueue
    /// variant). See the Linux variant for behavior.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    pub fn decode_one_event(bytes: &[u8]) -> Option<Event> {
        const N: usize = core::mem::size_of::<crate::sys::Kevent>();
        if bytes.len() < N {
            return None;
        }
        let mut buf = [0u8; N];
        buf.copy_from_slice(&bytes[..N]);
        // SAFETY: see Linux variant.
        let ev: crate::sys::Kevent = unsafe { core::ptr::read_unaligned(buf.as_ptr().cast()) };
        let ready = crate::sys::kevent_to_ready(&ev);
        let token = crate::sys::token_from_event(&ev);
        Some(Event::new(token, ready))
    }

    /// Construct an `Interest` from raw bits. Wraps the public API
    /// for fuzz drivers that need a single-byte-to-Interest mapping.
    pub fn interest_from_bits(b: u8) -> crate::Interest {
        let mut i = crate::Interest::empty();
        if b & 0b001 != 0 {
            i = i.add(crate::Interest::READABLE);
        }
        if b & 0b010 != 0 {
            i = i.add(crate::Interest::WRITABLE);
        }
        if b & 0b100 != 0 {
            i = i.add(crate::Interest::PRIORITY);
        }
        i
    }
}
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub use sys::udp_batch;
#[cfg(target_os = "linux")]
pub use sys::{Cqe, Ring, Sqe};
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
pub use sys::{RawFd, SourceFd};
pub use types::{Interest, IoError, IoErrorKind, IoResult, Ready, Token};

#[cfg(test)]
mod fuzz_helpers_tests {
    //! Unit tests exercising the `__fuzz_helpers` doc-hidden surface
    //! so coverage attributes test execution to it.

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    use crate::Interest;

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    #[test]
    fn decode_one_event_short_buffer_returns_none() {
        assert!(crate::__fuzz_helpers::decode_one_event(&[]).is_none());
        assert!(crate::__fuzz_helpers::decode_one_event(&[0u8; 3]).is_none());
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    #[test]
    fn decode_one_event_with_zeros_does_not_panic() {
        // 64 bytes covers every backend's struct size.
        let zeros = [0u8; 64];
        let ev =
            crate::__fuzz_helpers::decode_one_event(&zeros).expect("decode of zero-filled buffer");
        // Touch the decoded fields so optimizer can't elide.
        let _ = ev.token();
        let _ = ev.ready();
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    #[test]
    fn decode_one_event_with_fff_does_not_panic() {
        let raw = [0xffu8; 64];
        let ev =
            crate::__fuzz_helpers::decode_one_event(&raw).expect("decode of 0xff-filled buffer");
        let _ = ev.token();
        let _ = ev.ready();
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))]
    #[test]
    fn interest_from_bits_round_trip() {
        use crate::__fuzz_helpers::interest_from_bits;
        assert!(interest_from_bits(0).is_empty());
        assert!(interest_from_bits(0b001).contains(Interest::READABLE));
        assert!(!interest_from_bits(0b001).contains(Interest::WRITABLE));
        assert!(interest_from_bits(0b010).contains(Interest::WRITABLE));
        assert!(interest_from_bits(0b100).contains(Interest::PRIORITY));
        // High bits ignored — only low 3 bits map.
        let all = interest_from_bits(0b1111_0111);
        assert!(all.contains(Interest::READABLE));
        assert!(all.contains(Interest::WRITABLE));
        assert!(all.contains(Interest::PRIORITY));
    }
}
