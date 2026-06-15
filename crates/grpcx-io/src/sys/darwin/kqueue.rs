//! Darwin `kqueue` selector — readiness-based backend (mirror of
//! [`crate::sys::linux::epoll::Selector`]).
//!
//! Mapping from the cross-platform [`Interest`] bitset to kqueue
//! filters:
//!
//! | `Interest` bit | kqueue filter   |
//! |----------------|-----------------|
//! | `READABLE`     | `EVFILT_READ`   |
//! | `WRITABLE`     | `EVFILT_WRITE`  |
//! | `PRIORITY`     | `EVFILT_EXCEPT` |
//! | `AIO`          | *(not surfaced at L1; would map to `EVFILT_AIO`)* |
//!
//! Each (fd, filter) pair is a distinct kqueue registration. So a
//! single fd with `READABLE | WRITABLE` interest consumes two
//! kevents — not one event-mask like epoll.

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
        // kqueue() on Darwin does not honor an `O_CLOEXEC`-style
        // flag at create time; apply `FD_CLOEXEC` explicitly.
        if let Err(e) = raw::fcntl(kq, raw::F_SETFD, raw::FD_CLOEXEC) {
            let _ = raw::close(kq);
            return Err(e);
        }
        Ok(Self { kq })
    }

    /// Add the (fd, interest) pair to the kqueue.
    ///
    /// Applies up to three filters (`EVFILT_READ` / `EVFILT_WRITE`
    /// / `EVFILT_EXCEPT`) based on the interest bits. Uses
    /// `EV_CLEAR` so subsequent wakeups have edge-triggered
    /// semantics — matches mio for cross-OS parity.
    ///
    /// `EV_RECEIPT` is part of every change so `kevent` returns
    /// synchronously after processing the changelist (without
    /// blocking to drain pending events). Without it, the
    /// `timeout = NULL` path in [`apply_changes`] would block
    /// indefinitely.
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
    /// change list of exactly 3 entries (`ADD` slots first, then
    /// complement `DEL` slots).
    ///
    /// Layout:
    /// - For each filter in the new `interest`: `EV_ADD` (with
    ///   `EV_CLEAR | EV_RECEIPT`). `EV_ADD` is idempotent in
    ///   kqueue — it updates the token/udata in place if the
    ///   filter is already registered, or creates it otherwise.
    /// - For each filter **not** in the new `interest`:
    ///   `EV_DELETE` (with `EV_RECEIPT`). The kernel returns
    ///   `ENOENT` (which we ignore) if the filter wasn't set, or
    ///   removes it cleanly.
    ///
    /// Result: 3 entries / 1 syscall regardless of interest set
    /// (vs the v0.7 cut: 5 entries with `DEL` for all 3 plus
    /// `ADD` for the new ones, also vs the original v0.5 cut:
    /// 2 separate syscalls). Eliminates the "always DEL the
    /// filters we're about to ADD anyway" round-trip.
    pub(crate) fn reregister(
        &self,
        fd: raw::RawFd,
        token: Token,
        interest: Interest,
    ) -> IoResult<()> {
        let mut changes = [raw::Kevent::default(); 3];
        let mut n_add = 0;
        // ADD entries first (idempotent — refresh or create).
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
        if interest.contains(Interest::PRIORITY) {
            changes[n_add] = build_kevent(
                fd,
                token.0,
                raw::EVFILT_EXCEPT,
                raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
            );
            n_add += 1;
        }
        // DEL entries for filters NOT in the new interest set.
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
        if !interest.contains(Interest::PRIORITY) {
            changes[total] =
                build_kevent(fd, 0, raw::EVFILT_EXCEPT, raw::EV_DELETE | raw::EV_RECEIPT);
            total += 1;
        }

        let mut receipts = [raw::Kevent::default(); 3];
        let written = raw::kevent(self.kq, &changes[..total], &mut receipts[..total], None)?;
        evaluate_reregister_receipts(&receipts[..written], n_add)
    }

    /// Remove every filter we may have registered for `fd`.
    ///
    /// Returns [`IoErrorKind::NotFound`] when **every** filter
    /// `DELETE` reports `ENOENT` — i.e. the fd was never registered
    /// with this kqueue. Mirrors the v0.2 epoll contract so a second
    /// `deregister` of the same fd surfaces an error, not a silent
    /// success.
    ///
    /// `EV_RECEIPT` guarantees `kevent` returns synchronously after
    /// processing each change; a `NULL` timeout is fine here because
    /// the kernel does not actually wait — it just emits receipts.
    /// Empirically `NULL` is ~30× faster than `{0,0}` on macOS.
    pub(crate) fn deregister(&self, fd: raw::RawFd) -> IoResult<()> {
        let changes = [
            build_kevent(fd, 0, raw::EVFILT_READ, raw::EV_DELETE | raw::EV_RECEIPT),
            build_kevent(fd, 0, raw::EVFILT_WRITE, raw::EV_DELETE | raw::EV_RECEIPT),
            build_kevent(fd, 0, raw::EVFILT_EXCEPT, raw::EV_DELETE | raw::EV_RECEIPT),
        ];
        let mut receipts = [raw::Kevent::default(); 3];
        let written = raw::kevent(self.kq, &changes, &mut receipts, None)?;
        evaluate_deregister_receipts(&receipts[..written])
    }

    /// Wait until at least one event is ready (or `timeout`
    /// elapses). Retries on `EINTR` per the standard reactor
    /// convention.
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
    /// timeout is fine — no actual wait happens. Empirically the
    /// `NULL` path is ~30× faster than `{0,0}` on Darwin (one
    /// kqueue syscall: ~500 ns vs ~12 µs).
    fn apply_changes(&self, changes: &[raw::Kevent]) -> IoResult<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut receipts = [raw::Kevent::default(); 4];
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
    /// `token`. `EV_CLEAR` makes it auto-reset after each delivery, so
    /// the `Waker` needs no explicit drain.
    pub(crate) fn register_user(&self, ident: usize, token: Token) -> IoResult<()> {
        let change = raw::Kevent {
            ident,
            filter: raw::EVFILT_USER,
            flags: raw::EV_ADD | raw::EV_CLEAR | raw::EV_RECEIPT,
            fflags: 0,
            data: 0,
            udata: token.0,
        };
        let mut receipts = [raw::Kevent::default(); 1];
        let written = raw::kevent(self.kq, &[change], &mut receipts, None)?;
        scan_for_error(&receipts[..written])
    }

    /// Fire the `EVFILT_USER` event registered under `ident`. Safe to
    /// call from any thread (`kevent` on a shared kq is thread-safe).
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

/// Build a single `Kevent` for an `EV_ADD` or `EV_DELETE`
/// change.
fn build_kevent(fd: raw::RawFd, udata: usize, filter: i16, flags: u16) -> raw::Kevent {
    raw::Kevent {
        ident: fd as usize,
        filter,
        flags,
        fflags: 0,
        data: 0,
        udata,
    }
}

/// Materialize 1–3 [`raw::Kevent`] entries from an `Interest`.
fn interest_to_changes(
    fd: raw::RawFd,
    token: Token,
    interest: Interest,
    base_flags: u16,
) -> ([raw::Kevent; 3], usize) {
    let mut out = [raw::Kevent::default(); 3];
    let mut n = 0;
    if interest.contains(Interest::READABLE) {
        out[n] = build_kevent(fd, token.0, raw::EVFILT_READ, base_flags);
        n += 1;
    }
    if interest.contains(Interest::WRITABLE) {
        out[n] = build_kevent(fd, token.0, raw::EVFILT_WRITE, base_flags);
        n += 1;
    }
    if interest.contains(Interest::PRIORITY) {
        out[n] = build_kevent(fd, token.0, raw::EVFILT_EXCEPT, base_flags);
        n += 1;
    }
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
            // EV_ERROR with data=0 is the EV_RECEIPT "no error"
            // success token — skip.
            continue;
        }
        return Err(crate::types::IoError::new(errno_to_kind(errno), errno));
    }
    Ok(())
}

/// Inspect a reregister merged receipts list. First `n_add` entries
/// are strict `EV_ADD` receipts (any errno is a real failure); the
/// rest are `EV_DELETE` receipts where `ENOENT(2)` is tolerated
/// (the filter wasn't previously registered) but any other errno
/// fails.
///
/// - `errno == 0` everywhere is the `EV_RECEIPT` success marker.
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
        raw::EVFILT_EXCEPT => {
            ready = ready.add(Ready::PRIORITY);
        }
        raw::EVFILT_USER => {
            // A `Waker` wakeup. Surface it as readable so the event is
            // non-empty; the consumer keys on the token, not the bits.
            ready = ready.add(Ready::READABLE);
        }
        _ => {}
    }
    if ev.flags & raw::EV_EOF != 0 {
        // EV_EOF on a read filter means peer closed write side; on
        // a write filter it means our peer disappeared. Surface
        // both as HANGUP plus the half-close per filter.
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
