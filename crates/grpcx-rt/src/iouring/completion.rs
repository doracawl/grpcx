//! Completion-queue drain helper.
//!
//! [`Completion`] is the runtime-side view of one CQE
//! (`user_data`, `res`, `flags`); [`drain_cqes`] folds
//! them into a caller-supplied closure.
//!
//! Two cost profiles:
//!
//! - [`drain_cqes`] when a recent `submit_and_wait` (or
//!   external readiness signal) guarantees at least one
//!   CQE — returns the count drained.
//! - The caller can wrap [`drain_cqes`] inside a manual
//!   poll loop to interleave new SQE submissions with
//!   drain — see the transport_tcp_mux_v2 Linux branch
//!   for the integration pattern.

use grpcx_io::Cqe;

use super::ring::UringRing;

/// Runtime-side completion view.
#[derive(Clone, Copy, Debug)]
pub struct Completion {
    /// Cookie copied from the originating SQE's
    /// `user_data` field.
    pub user_data: u64,
    /// Kernel return value: non-negative on success
    /// (typically a byte count), negative `-errno` on
    /// failure.
    pub res: i32,
    /// CQE flag bits — see `IORING_CQE_F_*` in
    /// `linux/io_uring.h`.
    pub flags: u32,
}

impl From<Cqe> for Completion {
    #[inline]
    fn from(c: Cqe) -> Self {
        Self {
            user_data: c.user_data,
            res: c.res,
            flags: c.flags,
        }
    }
}

/// Drain every CQE currently visible on `ring`, invoking
/// `f` on each as a [`Completion`]. Returns the number
/// drained. A return of `0` means the kernel had no
/// completions ready — perfectly valid (call submit /
/// submit_and_wait to make some).
pub fn drain_cqes<F: FnMut(Completion)>(ring: &mut UringRing, mut f: F) -> u32 {
    ring.reap_cqes(|c| f(c.into()))
}
