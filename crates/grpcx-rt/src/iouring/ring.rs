//! Owning [`UringRing`] handle — the runtime-level wrapper
//! around `grpcx_io::Ring`. Adds the SQPOLL + SQ_AFF
//! convenience constructor plus a single-shot drain loop
//! used by [`super::completion::drain_cqes`].

use grpcx_io::{Cqe, Ring, Sqe};

/// Owning io_uring instance configured for the v2
/// transport's submit / drain pattern.
///
/// Wraps a `grpcx_io::Ring` and gives it a friendlier
/// name at the runtime layer; no extra state beyond what
/// `Ring` already tracks. Construct with [`Self::new`]
/// for the standard mode or [`Self::new_sqpoll`] when the
/// caller wants the kernel polling thread on a pinned CPU.
pub struct UringRing {
    inner: Ring,
    has_sqpoll: bool,
}

impl UringRing {
    /// Plain ring with `entries` SQ slots and the default
    /// (non-SQPOLL) submission discipline. Surfaces the
    /// underlying `Ring::new` errors verbatim — including
    /// `IoErrorKind::BackendUnavailable` on kernels < 5.1.
    pub fn new(entries: u32) -> grpcx_io::IoResult<Self> {
        Ring::new(entries).map(|r| Self {
            inner: r,
            has_sqpoll: false,
        })
    }

    /// SQPOLL-mode ring pinned to logical CPU `cpu`, with
    /// the polling thread sleeping after `idle_ms` of
    /// inactivity. See [`Ring::new_sqpoll`] for the
    /// kernel-version + capability prerequisites; a
    /// kernel refusal surfaces as
    /// [`grpcx_io::IoErrorKind::BackendUnavailable`] so
    /// the caller can fall back to [`Self::new`].
    pub fn new_sqpoll(entries: u32, cpu: u32, idle_ms: u32) -> grpcx_io::IoResult<Self> {
        Ring::new_sqpoll(entries, cpu, idle_ms).map(|r| Self {
            inner: r,
            has_sqpoll: true,
        })
    }

    /// Whether this ring was created with SQPOLL — affects
    /// which submit primitive is cheapest (see
    /// [`Self::submit`]).
    #[inline]
    pub fn has_sqpoll(&self) -> bool {
        self.has_sqpoll
    }

    /// Push a [`Sqe`] onto the submission queue. The SQE
    /// is not visible to the kernel until [`Self::submit`]
    /// (or [`Self::submit_and_wait`]) is called.
    #[inline]
    pub fn push_sqe(&mut self, sqe: &Sqe) -> grpcx_io::IoResult<()> {
        self.inner.push_sqe(sqe)
    }

    /// Submit any pending SQEs. On a SQPOLL ring this is
    /// the cheap `submit_sqpoll` (zero syscalls when the
    /// kernel thread is awake); on a non-SQPOLL ring it
    /// is the regular `submit` wrapper.
    #[inline]
    pub fn submit(&self, to_submit: u32) -> grpcx_io::IoResult<i32> {
        if self.has_sqpoll {
            self.inner.submit_sqpoll(to_submit)
        } else {
            self.inner.submit(to_submit)
        }
    }

    /// Submit and block in the kernel until `min_complete`
    /// CQEs are available. The SQPOLL fast path does not
    /// participate here — `min_complete > 0` always needs
    /// a real syscall.
    #[inline]
    pub fn submit_and_wait(&self, to_submit: u32, min_complete: u32) -> grpcx_io::IoResult<i32> {
        self.inner.submit_and_wait(to_submit, min_complete)
    }

    /// Drain every available CQE, invoking `f` on each.
    /// Returns the number drained.
    #[inline]
    pub fn reap_cqes<F: FnMut(Cqe)>(&mut self, f: F) -> u32 {
        self.inner.reap_cqes(f)
    }

    /// Borrow the underlying `grpcx_io::Ring` so the
    /// register / file-table helpers in [`super::register`]
    /// can reach the ring-fd API surface.
    pub(crate) fn inner(&self) -> &Ring {
        &self.inner
    }
}
