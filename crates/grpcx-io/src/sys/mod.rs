//! Platform-specific backend implementations.
//!
//! Selected at compile time via `cfg(target_os)`. v0.2 shipped only
//! the Linux backend; v0.4 added Linux io_uring; v0.5 added macOS /
//! iOS via kqueue; v0.7 adds FreeBSD via kqueue. Other targets
//! compile but contain no backend (every reactor call returns
//! `BackendUnavailable`).
//!
//! Items here are `pub(crate)`-only; the public face of the reactor
//! lives in `crate::reactor` / `crate::source` / `crate::event`.

// Temporarily allowed during the v0.2 wiring sequence (T6 → T9).
// Each item under this tree becomes live as soon as `Poll` /
// `Registry` start calling into it (T9). At v0.2 close this attribute
// is removed.
#![allow(dead_code)]

#[cfg(target_os = "linux")]
mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod darwin;

#[cfg(target_os = "freebsd")]
mod freebsd;

#[cfg(target_os = "linux")]
pub(crate) use linux::{EpollEvent, Selector, Waker, epoll_events_to_ready, read_event};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use darwin::{Kevent, Selector, Timespec, Waker, kevent_to_ready, token_from_event};

#[cfg(target_os = "freebsd")]
pub(crate) use freebsd::{Kevent, Selector, Timespec, Waker, kevent_to_ready, token_from_event};

#[cfg(target_os = "linux")]
pub use linux::{Cqe, RawFd, Ring, SourceFd, Sqe, udp_batch};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use darwin::{RawFd, SourceFd, udp_batch};

#[cfg(target_os = "freebsd")]
pub use freebsd::{RawFd, SourceFd};

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use linux::__bench_helpers;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[doc(hidden)]
pub use darwin::__bench_helpers;

#[cfg(target_os = "freebsd")]
#[doc(hidden)]
pub use freebsd::__bench_helpers;
