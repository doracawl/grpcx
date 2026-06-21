#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![no_std]

//! Layout: the `current_thread` executor ([`block_on`], [`spawn`],
//! [`JoinHandle`], [`Spawn`], the [`readable`] reactor primitive), the
//! time surface ([`sleep`] / [`Sleep`], [`Timer`], [`timeout`] /
//! [`Timeout`] / [`Elapsed`], [`interval`] / [`Interval`]), and the
//! channels ([`channel`] / [`Sender`] / [`Receiver`] mpsc, [`oneshot`])
//! are std-backed and implemented as of v0.4, atop the `no_std` timer
//! wheel in the `timer` module. Cross-thread channel sends unpark the
//! driver via `grpcx_io::Waker`.
//!
//! The multi-thread work-stealing [`Runtime`] (v0.5) adds a pool of
//! worker threads over the `no_std` lock-free Chase-Lev deque in the
//! `deque` module, plus a dedicated reactor/timer driver thread. It runs
//! on a separate track from the current-thread executor: spawn via
//! [`Runtime::spawn`] / [`RtHandle`], drive a root with
//! [`Runtime::block_on`]. The shared `sleep` / `readable` primitives
//! dispatch to whichever runtime is current; [`yield_now`] plus a
//! cooperative budget keep a busy task from starving its worker's peers.
//!
//! See `../../docs/v2-scope.md` and the per-checkpoint `REFACTOR-v2-*.md`.

extern crate alloc;

mod channel;
mod deque;
mod executor;
#[cfg(all(feature = "iouring", target_os = "linux"))]
pub mod iouring;
mod runtime;
mod timer;

#[doc(hidden)]
pub mod __fuzz_helpers;

pub use channel::{Receiver, Recv, Sender, TryRecvError, TrySendError, channel, oneshot};
pub use executor::{
    Elapsed, Handle, Interval, JoinHandle, Readable, Sleep, Spawn, Timeout, Timer, block_on,
    interval, readable, sleep, spawn, timeout,
};
pub use runtime::{RtHandle, Runtime, YieldNow, yield_now};
