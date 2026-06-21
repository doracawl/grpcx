//! io_uring completion-based runtime backing
//! `transport-tcp-mux v2`'s Linux fast path (T4.2 of
//! `_dev/IMPLEMENTATION-PLAN.md`).
//!
//! Sits sibling to the v1.0 readiness-based reactor
//! (`grpcx_io::Poll`); the two backends ship side-by-side
//! and the call-site picks which one to use. Same shape
//! as `grpcx_io::sys::linux::io_uring` (completion model,
//! not readiness) — this module layers the runtime-level
//! abstractions on top:
//!
//! - [`ring`] — owning [`UringRing`] handle plus the
//!   SQPOLL + SQ_AFF set-up.
//! - [`register`] — fixed-file + buffer-table registration
//!   (`IORING_REGISTER_FILES` / `IORING_REGISTER_BUFFERS`).
//! - [`ops`] — typed SQE constructors (send / recv /
//!   `RECV_MULTISHOT` / linked send pair / fixed-file
//!   variants).
//! - [`completion`] — CQE drain helper.
//!
//! Compiled only on `cfg(target_os = "linux")` — on
//! macOS / FreeBSD `cargo build --features iouring` is a
//! no-op (the module is gone, the entry points fall back
//! to the kqueue/epoll path at the call-site). No
//! `unimplemented!()` panics anywhere on a non-Linux
//! build.

#![cfg(target_os = "linux")]

pub mod completion;
pub mod ops;
pub mod register;
pub mod ring;

pub use completion::{Completion, drain_cqes};
pub use ops::{
    submit_linked_send_pair, submit_recv, submit_recv_multishot, submit_send, submit_send_fixed,
};
pub use register::{
    FixedFileTable, RegisteredBuffers, register_buffers, register_files, unregister_buffers,
    unregister_files,
};
pub use ring::UringRing;
