//! Linux `io_uring` completion-based reactor backend.
//!
//! Sits **sibling** to `epoll`, not a substitute for it. Readiness
//! polling (epoll) and completion notification (io_uring) are two
//! different programming models with different cost profiles; grpcx-io
//! exposes both at L1 and lets the caller pick.
//!
//! T18 → T21:
//! - T18 — bare ring fd + the three mmap regions ([`Ring`])
//! - T19 — [`Sqe`] constructors (`nop` / `read` / `write`)
//! - T20 — queue ops on `Ring` (`push_sqe` / `submit` / `submit_and_wait`
//!   / `reap_cqes`) and the [`Cqe`] value type
//! - T21 — promote `Ring` / `Sqe` / `Cqe` to the crate's public API

mod entries;
mod ring;

#[cfg(test)]
mod tests;

pub use entries::{Cqe, Sqe};
pub use ring::Ring;
