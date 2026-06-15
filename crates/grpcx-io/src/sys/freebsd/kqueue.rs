//! FreeBSD `kqueue` selector — readiness-based backend (mirror of
//! [`crate::sys::darwin::kqueue::Selector`]).
//!
//! The shape is intentionally identical to the Darwin selector; only
//! the underlying `raw::Kevent` struct differs (FreeBSD 12+ has an
//! `ext[4]` tail). The conformance contract is the same — see the
//! Darwin doc for `Interest` → filter mapping.
//!
//! # FreeBSD-specific deviation
//!
//! FreeBSD has no `EVFILT_EXCEPT`. [`Interest::PRIORITY`] is
//! **silently dropped** at registration time — the registration
//! succeeds and never raises a PRIORITY readiness event from the
//! kernel. This mirrors how mio's FreeBSD backend handles the same
//! gap and lets cross-platform code targeting `linux + macos +
//! freebsd` ignore the difference.

use super::raw;
use crate::types::{Interest, IoErrorKind, IoResult, Ready, Token};

/// `Selector` wraps the kernel kq fd.
///
/// Created once by `Poll::new()`, shared through `Registry` clones.
/// Drop closes the underlying fd.
pub(crate) struct Selector {
    kq: raw::RawFd,
}

impl Selector {
    /// Open a new kqueue. Wraps `kqueue(2)`; sets `FD_CLOEXEC` so
    /// the kq fd does not leak into child processes via `exec*`.
    pub(crate) fn new() -> IoResult<Self> {
        let kq = raw::kqueue()?;
        if let Err(e) = raw::fcntl(kq, raw::F_SETFD, raw::FD_CLOEXEC) {
            let _ = raw::close(kq);
            return Err(e);
        }
        Ok(Self { kq })
    }

    /// Add the (fd, interest) pair to the kqueue.
    ///
    /// Applies up to two filters (`EVFILT_READ` / `EVFILT_WRITE`)
    /// based on the interest bits — `PRIORITY` is dropped (FreeBSD
    /// has no `EVFILT_EXCEPT`). Uses `EV_CLEAR` for edge-triggered
    /// semantics matching the cross-OS contract.
    ///
    /// `EV_RECEIPT` is part of every change so `kevent` returns
    /// synchronously after processing the changelist without
    /// blocking; without it the `timeout = NULL` path would block
    /// indefinitely waiting for events to drain.
    pub(crate) fn register(
        &self,
        fd: raw::RawFd,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        let (changes, n) = interest_to_changes(
            fd,
            token,
            interest,
            raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
        );
        self.apply_changes(&changes[..n])
    }

    /// Reregister via a **single `kevent` syscall** with a
    /// change list of exactly 2 entries (`ADD` slots first, then
    /// complement `DEL` slots). See the Darwin variant for the
    /// full rationale.
    ///
    /// FreeBSD has no `EVFILT_EXCEPT`, so the change list is at
    /// most 2 entries (Darwin's variant carries up to 3).
    pub(crate) fn reregister(
        &self,
        fd: raw::RawFd,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        let mut changes = [raw::Kevent::default(); 2];
        let mut n_add = 0;
        // ADD entries first (idempotent in kqueue — refresh or create).
        if interest.contains(Interest::READABLE) {
            changes[n_add] = build_kevent(
                fd,
                token.0,
                raw::EVFILT_READ,
                raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
            );
            n_add += 1;
        }
        if interest.contains(Interest::WRITABLE) {
            changes[n_add] = build_kevent(
                fd,
                token.0,
                raw::EVFILT_WRITE,
                raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
            );
            n_add += 1;
        }
        // DEL entries for filters NOT in new interest.
        let mut total = n_add;
        if !interest.contains(Interest::READABLE) {
            changes[total] =
                build_kevent(fd, 0, raw::EVFILT_READ, raw::EV_DELETE | raw::EV_RECEIPT);
            total += 1;
        }
        if !interest.contains(Interest::WRITABLE) {
            changes[total] =
                build_kevent(fd, 0, raw::EVFILT_WRITE, raw::EV_DELETE | raw::EV_RECEIPT);
            total += 1;
        }

        let mut receipts = [raw::Kevent::default(); 2];
        let written = raw::kevent(self.kq, &changes[..total], &mut receipts[..total], None)?;
        evaluate_reregister_receipts(&receipts[..written], n_add)
    }

    /// Remove every filter we may have registered for `fd`.
    ///
    /// Returns [`IoErrorKind::NotFound`] when **every** filter
    /// `DELETE` reports `ENOENT` — i.e. the fd was never registered
    /// with this kqueue. Mirrors the v0.2 epoll contract so a
    /// second `deregister` of the same fd surfaces an error.
    ///
    /// We submit DELETE for READ and WRITE only (no EVFILT_EXCEPT
    /// on FreeBSD), so the receipt list has at most 2 entries.
    pub(crate) fn deregister(&self, fd: raw::RawFd) -> IoResult<()> {
        let changes = [
            build_kevent(fd, 0, raw::EVFILT_READ, raw::EV_DELETE | raw::EV_RECEIPT),
            build_kevent(fd, 0, raw::EVFILT_WRITE, raw::EV_DELETE | raw::EV_RECEIPT),
        ];
        let mut receipts = [raw::Kevent::default(); 2];
        let written = raw::kevent(self.kq, &changes, &mut receipts, None)?;
        evaluate_deregister_receipts(&receipts[..written])
    }

    /// Wait until at least one event is ready (or `timeout`
    /// elapses). Retries on `EINTR`.
    pub(crate) fn wait(
        &self,
        events: &mut [raw::Kevent],
        timeout: Option<raw::Timespec>,
    ) -> IoResult<usize> {
        loop {
            match raw::kevent(self.kq, &[], events, timeout.as_ref()) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == IoErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Apply a change list. With `EV_RECEIPT` the kernel emits one
    /// receipt per change and returns synchronously, so a `NULL`
    /// timeout is fine — no actual wait happens.
    fn apply_changes(&self, changes: &[raw::Kevent]) -> IoResult<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut receipts = [raw::Kevent::default(); 3];
        let n = receipts.len().min(changes.len());
        let written = raw::kevent(self.kq, changes, &mut receipts[..n], None)?;
        scan_for_error(&receipts[..written])
    }

    // ------------------------------------------------------------
    // ctl_* aliases — mirror the Linux Selector's API surface so
    // `reactor::Registry` can dispatch through the same method
    // names.
    // ------------------------------------------------------------

    pub(crate) fn ctl_add(&self, fd: raw::RawFd, token: Token, interest: Interest) -> IoResult<()> {
        self.register(fd, token, interest)
    }

    pub(crate) fn ctl_mod(&self, fd: raw::RawFd, token: Token, interest: Interest) -> IoResult<()> {
        self.reregister(fd, token, interest)
    }

    pub(crate) fn ctl_del(&self, fd: raw::RawFd) -> IoResult<()> {
        self.deregister(fd)
    }

    // ------------------------------------------------------------
    // EVFILT_USER — backend for the cross-thread `Waker`.
    // ------------------------------------------------------------

    /// Register a user-triggerable event under `ident`, tagged with
    /// `token`. `EV_CLEAR` auto-resets it after each delivery.
    pub(crate) fn register_user(&self, ident: usize, token: Token) -> IoResult<()> {
        let change = raw::Kevent {
            ident,
            filter: raw::EVFILT_USER,
            flags: raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
            fflags: 0,
            data: 0,
            udata: token.0,
            ext: [0; 4],
        };
        let mut receipts = [raw::Kevent::default(); 1];
        let written = raw::kevent(self.kq, &[change], &mut receipts, None)?;
        scan_for_error(&receipts[..written])
    }

    /// Fire the `EVFILT_USER` event under `ident`. Thread-safe.
    ///
    /// `udata` must repeat the token from [`register_user`]: a trigger
    /// change overwrites the knote's `udata`, so passing it through
    /// keeps the delivered event's token correct.
    pub(crate) fn trigger_user(&self, ident: usize, udata: usize) -> IoResult<()> {
        let change = raw::Kevent {
            ident,
            filter: raw::EVFILT_USER,
            flags: raw::EV_RECEIPT,
            fflags: raw::NOTE_TRIGGER,
            data: 0,
            udata,
            ext: [0; 4],
        };
        let mut receipts = [raw::Kevent::default(); 1];
        let written = raw::kevent(self.kq, &[change], &mut receipts, None)?;
        scan_for_error(&receipts[..written])
    }

    /// Remove the user event under `ident`. Tolerates `ENOENT`.
    pub(crate) fn deregister_user(&self, ident: usize) -> IoResult<()> {
        let change = raw::Kevent {
            ident,
            filter: raw::EVFILT_USER,
            flags: raw::EV_DELETE | raw::EV_RECEIPT,
            fflags: 0,
            data: 0,
            udata: 0,
            ext: [0; 4],
        };
        let mut receipts = [raw::Kevent::default(); 1];
        let written = raw::kevent(self.kq, &[change], &mut receipts, None)?;
        for ev in &receipts[..written] {
            if ev.flags & raw::EV_ERROR != 0 {
                let errno = ev.data as i32;
                if errno != 0 && errno != 2 {
                    return Err(crate::types::IoError::new(errno_to_kind(errno), errno));
                }
            }
        }
        Ok(())
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        let _ = raw::close(self.kq);
    }
}

/// Build a single `Kevent` for an `EV_ADD` or `EV_DELETE` change.
fn build_kevent(fd: raw::RawFd, udata: usize, filter: i16, flags: u16) -> raw::Kevent {
    raw::Kevent {
        ident: fd as usize,
        filter,
        flags,
        fflags: 0,
        data: 0,
        udata,
        ext: [0; 4],
    }
}

/// Materialize 1–2 [`raw::Kevent`] entries from an `Interest`.
///
/// `PRIORITY` is dropped — FreeBSD has no `EVFILT_EXCEPT`.
fn interest_to_changes(
    fd: raw::RawFd,
    token: Token,
    interest: Interest,
    base_flags: u16,
) -> ([raw::Kevent; 2], usize) {
    let mut out = [raw::Kevent::default(); 2];
    let mut n = 0;
    if interest.contains(Interest::READABLE) {
        out[n] = build_kevent(fd, token.0, raw::EVFILT_READ, base_flags);
        n += 1;
    }
    if interest.contains(Interest::WRITABLE) {
        out[n] = build_kevent(fd, token.0, raw::EVFILT_WRITE, base_flags);
        n += 1;
    }
    // Interest::PRIORITY silently dropped — see module-level note.
    (out, n)
}

/// Scan a receipt list for `EV_ERROR` entries — used by the
/// register / reregister paths. Returns the first non-zero error.
fn scan_for_error(receipts: &[raw::Kevent]) -> IoResult<()> {
    for ev in receipts {
        if ev.flags & raw::EV_ERROR == 0 {
            continue;
        }
        let errno = ev.data as i32;
        if errno == 0 {
            continue;
        }
        return Err(crate::types::IoError::new(errno_to_kind(errno), errno));
    }
    Ok(())
}

/// Inspect a reregister merged receipts list. First `n_add` entries
/// are `EV_ADD` slots (strict — any errno is a real failure); the
/// rest are `EV_DELETE` slots where `ENOENT(2)` is tolerated.
fn evaluate_reregister_receipts(receipts: &[raw::Kevent], n_add: usize) -> IoResult<()> {
    for (i, ev) in receipts.iter().enumerate() {
        if ev.flags & raw::EV_ERROR == 0 {
            continue;
        }
        let errno = ev.data as i32;
        match errno {
            0 => {}               // EV_RECEIPT success marker
            2 if i >= n_add => {} // ENOENT on DEL slot — fine
            e => return Err(crate::types::IoError::new(errno_to_kind(e), e)),
        }
    }
    Ok(())
}

/// Inspect a deregister-receipts list. The contract:
///
/// - At least one filter `DELETE` succeeded → `Ok(())`. The fd had
///   some subset of filters and we removed them.
/// - Every filter `DELETE` reported `ENOENT` → `NotFound`. The fd
///   was never registered.
/// - Any other errno → propagate that.
fn evaluate_deregister_receipts(receipts: &[raw::Kevent]) -> IoResult<()> {
    let mut any_success = false;
    let mut any_enoent = false;
    for ev in receipts {
        if ev.flags & raw::EV_ERROR == 0 {
            continue;
        }
        let errno = ev.data as i32;
        match errno {
            0 => any_success = true,
            2 => any_enoent = true, // ENOENT
            e => return Err(crate::types::IoError::new(errno_to_kind(e), e)),
        }
    }
    if !any_success && any_enoent {
        return Err(crate::types::IoError::new(IoErrorKind::NotFound, 2));
    }
    Ok(())
}

fn errno_to_kind(errno: i32) -> IoErrorKind {
    match errno {
        2 => IoErrorKind::NotFound,
        9 => IoErrorKind::InvalidInput,       // EBADF
        17 => IoErrorKind::AlreadyRegistered, // EEXIST
        22 => IoErrorKind::InvalidInput,      // EINVAL
        _ => IoErrorKind::Other,
    }
}

/// Translate a kqueue event's `filter` + `flags` into the public
/// [`Ready`] bitset.
pub(crate) fn kevent_to_ready(ev: &raw::Kevent) -> Ready {
    let mut ready = Ready::empty();
    match ev.filter {
        raw::EVFILT_READ => {
            ready = ready.add(Ready::READABLE);
        }
        raw::EVFILT_WRITE => {
            ready = ready.add(Ready::WRITABLE);
        }
        raw::EVFILT_USER => {
            // A `Waker` wakeup; surface as readable (consumer keys on
            // the token, not the bits).
            ready = ready.add(Ready::READABLE);
        }
        _ => {}
    }
    if ev.flags & raw::EV_EOF != 0 {
        match ev.filter {
            raw::EVFILT_READ => {
                ready = ready.add(Ready::READ_CLOSED);
            }
            raw::EVFILT_WRITE => {
                ready = ready.add(Ready::WRITE_CLOSED);
            }
            _ => {}
        }
        ready = ready.add(Ready::HANGUP);
    }
    if ev.flags & raw::EV_ERROR != 0 {
        ready = ready.add(Ready::ERROR);
    }
    ready
}

/// Recover the user [`Token`] from a kqueue event's `udata` slot.
#[inline]
pub(crate) fn token_from_event(ev: &raw::Kevent) -> Token {
    Token(ev.udata)
}
