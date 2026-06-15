//! Core value types used across the readiness API.

use core::fmt;
use core::num::NonZeroI32;
use core::ops::BitOr;

/// Opaque user-supplied label attached to a registered source.
///
/// The reactor returns events tagged with the token the source was
/// registered with. The caller chooses how to map a token back to its
/// state — typically an index into a slab / vector.
///
/// `usize` is wide enough on every supported platform to hold a
/// pointer-tagged or table-indexed identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Token(pub usize);

/// What kinds of readiness the caller wants notified for a source.
///
/// `Interest` is a non-empty set; reactor implementations may reject
/// an empty interest at register time with
/// [`IoErrorKind::InvalidInput`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interest(u8);

impl Interest {
    /// Notify when the source has bytes / a connection / equivalent
    /// ready to read without blocking.
    pub const READABLE: Self = Self(0b0000_0001);
    /// Notify when the source can accept writes without blocking.
    pub const WRITABLE: Self = Self(0b0000_0010);
    /// Notify on prioritized / out-of-band readability (TCP urgent).
    pub const PRIORITY: Self = Self(0b0000_0100);
    /// Notify on AIO-style completion (`kqueue` `EVFILT_AIO`, etc.).
    ///
    /// Reserved for backends that surface it directly; `epoll` has no
    /// equivalent and ignores this bit.
    pub const AIO: Self = Self(0b0000_1000);

    /// The empty interest set (will be rejected at registration).
    pub const fn empty() -> Self {
        Self(0)
    }

    /// True iff no bits are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True iff every bit in `other` is also set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set union.
    pub const fn add(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Set difference (`self` minus `other`).
    pub const fn remove(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl BitOr for Interest {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.add(rhs)
    }
}

impl fmt::Debug for Interest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut emit = |name: &str, f: &mut fmt::Formatter<'_>| -> fmt::Result {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str(name)
        };
        if self.contains(Self::READABLE) {
            emit("READABLE", f)?;
        }
        if self.contains(Self::WRITABLE) {
            emit("WRITABLE", f)?;
        }
        if self.contains(Self::PRIORITY) {
            emit("PRIORITY", f)?;
        }
        if self.contains(Self::AIO) {
            emit("AIO", f)?;
        }
        if first {
            f.write_str("(empty)")?;
        }
        Ok(())
    }
}

/// What readiness the reactor observed on this event.
///
/// Distinct from [`Interest`]: `Interest` is a request, `Ready` is an
/// observation. The bits include error / hangup / read-closed because
/// some backends surface those independently of `READABLE` /
/// `WRITABLE`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ready(u16);

impl Ready {
    /// Bytes / a connection / equivalent are available to read.
    pub const READABLE: Self = Self(0b0000_0000_0000_0001);
    /// Source can accept writes without blocking.
    pub const WRITABLE: Self = Self(0b0000_0000_0000_0010);
    /// Out-of-band / urgent data is readable.
    pub const PRIORITY: Self = Self(0b0000_0000_0000_0100);
    /// AIO-style completion observed.
    pub const AIO: Self = Self(0b0000_0000_0000_1000);
    /// Peer half-closed the read side (POSIX `EPOLLRDHUP`).
    pub const READ_CLOSED: Self = Self(0b0000_0000_0001_0000);
    /// Peer half-closed the write side.
    pub const WRITE_CLOSED: Self = Self(0b0000_0000_0010_0000);
    /// Generic error condition (`POLLERR` / `EVFILT` exception).
    pub const ERROR: Self = Self(0b0000_0000_0100_0000);
    /// Hangup; peer fully closed.
    pub const HANGUP: Self = Self(0b0000_0000_1000_0000);

    /// The empty readiness observation.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// True iff no readiness bits are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True iff every bit in `other` is also set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set union.
    pub const fn add(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr for Ready {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.add(rhs)
    }
}

impl fmt::Debug for Ready {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut emit = |name: &str, f: &mut fmt::Formatter<'_>| -> fmt::Result {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str(name)
        };
        if self.contains(Self::READABLE) {
            emit("READABLE", f)?;
        }
        if self.contains(Self::WRITABLE) {
            emit("WRITABLE", f)?;
        }
        if self.contains(Self::PRIORITY) {
            emit("PRIORITY", f)?;
        }
        if self.contains(Self::AIO) {
            emit("AIO", f)?;
        }
        if self.contains(Self::READ_CLOSED) {
            emit("READ_CLOSED", f)?;
        }
        if self.contains(Self::WRITE_CLOSED) {
            emit("WRITE_CLOSED", f)?;
        }
        if self.contains(Self::ERROR) {
            emit("ERROR", f)?;
        }
        if self.contains(Self::HANGUP) {
            emit("HANGUP", f)?;
        }
        if first {
            f.write_str("(empty)")?;
        }
        Ok(())
    }
}

/// Errors returned from reactor operations.
///
/// Backends map their native error codes (`errno` on POSIX,
/// `GetLastError` on Windows) into the [`IoErrorKind`] taxonomy. The
/// raw OS code is preserved when available for debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoError {
    kind: IoErrorKind,
    raw_os_code: Option<NonZeroI32>,
}

impl IoError {
    /// Build a new error from a kind and a raw OS code.
    ///
    /// `raw_os_code` of `0` means "no underlying OS code".
    pub const fn new(kind: IoErrorKind, raw_os_code: i32) -> Self {
        Self {
            kind,
            raw_os_code: NonZeroI32::new(raw_os_code),
        }
    }

    /// The categorized kind of this error.
    pub const fn kind(&self) -> IoErrorKind {
        self.kind
    }

    /// The raw OS error code, if one was reported.
    pub const fn raw_os_code(&self) -> Option<i32> {
        match self.raw_os_code {
            Some(c) => Some(c.get()),
            None => None,
        }
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind)?;
        if let Some(c) = self.raw_os_code() {
            write!(f, " (os error {c})")?;
        }
        Ok(())
    }
}

/// Categorized kinds of reactor errors.
///
/// `#[non_exhaustive]`: backends may introduce more specific kinds
/// without a SemVer break.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoErrorKind {
    /// A `register` was attempted on a source already registered with
    /// this reactor.
    AlreadyRegistered,
    /// A `reregister` / `deregister` was attempted on a source the
    /// reactor does not know.
    NotFound,
    /// One of the inputs (e.g. an empty [`Interest`]) was rejected.
    InvalidInput,
    /// The underlying syscall was interrupted (POSIX `EINTR`).
    Interrupted,
    /// Non-blocking operation would block.
    WouldBlock,
    /// Insufficient permissions.
    PermissionDenied,
    /// Out of memory in the kernel or process.
    OutOfMemory,
    /// No backend is wired in this build. v0.1 returns this from
    /// every fallible reactor call.
    BackendUnavailable,
    /// The peer actively refused (POSIX `ECONNREFUSED` —
    /// on connected UDP sockets this is the kernel
    /// surfacing an ICMP port-unreachable).
    ConnectionRefused,
    /// Catch-all for kinds not yet promoted to a specific variant.
    Other,
}

/// Shorthand for `Result<T, IoError>`.
pub type IoResult<T> = core::result::Result<T, IoError>;

// ============================================================
// Static struct-size assertions — `BUDGETS.md` MEM tier anchors.
// Catches accidental field-layout drift in `cargo check`.
// ============================================================
const _: () = assert!(core::mem::size_of::<Token>() == core::mem::size_of::<usize>());
const _: () = assert!(core::mem::size_of::<Interest>() == 1);
const _: () = assert!(core::mem::size_of::<Ready>() == 2);
// `IoError` = `IoErrorKind` (1-byte enum + niche) + `Option<NonZeroI32>` (4 B; niche-packed inside the Option) → 8 B total on aligned-to-4 targets.
const _: () = assert!(core::mem::size_of::<IoError>() == 8);
const _: () = assert!(core::mem::size_of::<IoErrorKind>() == 1);

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    // ----- Token -----

    #[test]
    fn token_construct_and_debug() {
        let t = Token(42);
        assert_eq!(t.0, 42);
        // Hash + Ord + Eq via derive
        assert!(t == Token(42));
        assert!(t < Token(43));
        assert!(t > Token(41));
        // Debug formatting goes through derive — exercise it.
        let s = std::format!("{:?}", t);
        assert!(s.contains("42"));
    }

    // ----- Interest -----

    #[test]
    fn interest_constants_distinct_bits() {
        assert_ne!(Interest::READABLE.0, Interest::WRITABLE.0);
        assert_ne!(Interest::READABLE.0, Interest::PRIORITY.0);
        assert_ne!(Interest::READABLE.0, Interest::AIO.0);
        assert_eq!(
            Interest::READABLE.0 | Interest::WRITABLE.0 | Interest::PRIORITY.0 | Interest::AIO.0,
            0b1111
        );
    }

    #[test]
    fn interest_empty_and_is_empty() {
        let e = Interest::empty();
        assert!(e.is_empty());
        assert!(!Interest::READABLE.is_empty());
    }

    #[test]
    fn interest_contains() {
        let rw = Interest::READABLE.add(Interest::WRITABLE);
        assert!(rw.contains(Interest::READABLE));
        assert!(rw.contains(Interest::WRITABLE));
        assert!(!rw.contains(Interest::PRIORITY));
        // Reflexive
        assert!(Interest::READABLE.contains(Interest::READABLE));
        // Empty trivially contained
        assert!(Interest::READABLE.contains(Interest::empty()));
    }

    #[test]
    fn interest_add_idempotent_and_remove() {
        let r = Interest::READABLE;
        let rr = r.add(Interest::READABLE);
        assert_eq!(r.0, rr.0); // add of same bit is no-op
        let rw = r.add(Interest::WRITABLE);
        assert!(rw.contains(Interest::READABLE));
        let w_only = rw.remove(Interest::READABLE);
        assert!(!w_only.contains(Interest::READABLE));
        assert!(w_only.contains(Interest::WRITABLE));
    }

    #[test]
    fn interest_bitor_eq_add() {
        let rw_op = Interest::READABLE | Interest::WRITABLE;
        let rw_fn = Interest::READABLE.add(Interest::WRITABLE);
        assert_eq!(rw_op.0, rw_fn.0);
    }

    #[test]
    fn interest_debug_renders_named_bits() {
        let s = std::format!("{:?}", Interest::READABLE);
        assert_eq!(s, "READABLE");
        let s = std::format!("{:?}", Interest::READABLE | Interest::WRITABLE);
        assert_eq!(s, "READABLE | WRITABLE");
        let s = std::format!(
            "{:?}",
            Interest::READABLE | Interest::WRITABLE | Interest::PRIORITY | Interest::AIO
        );
        assert_eq!(s, "READABLE | WRITABLE | PRIORITY | AIO");
        let s = std::format!("{:?}", Interest::empty());
        assert_eq!(s, "(empty)");
    }

    // ----- Ready -----

    #[test]
    fn ready_constants_distinct_bits() {
        let all = Ready::READABLE
            .add(Ready::WRITABLE)
            .add(Ready::PRIORITY)
            .add(Ready::AIO)
            .add(Ready::READ_CLOSED)
            .add(Ready::WRITE_CLOSED)
            .add(Ready::ERROR)
            .add(Ready::HANGUP);
        assert_eq!(all.0, 0b1111_1111);
    }

    #[test]
    fn ready_empty_contains_add_bitor() {
        let e = Ready::empty();
        assert!(e.is_empty());
        assert!(!Ready::READABLE.is_empty());

        let rw = Ready::READABLE | Ready::WRITABLE;
        assert_eq!(rw.0, Ready::READABLE.add(Ready::WRITABLE).0);
        assert!(rw.contains(Ready::READABLE));
        assert!(!rw.contains(Ready::HANGUP));
    }

    #[test]
    fn ready_debug_renders_all_named_bits() {
        let all = Ready::READABLE
            | Ready::WRITABLE
            | Ready::PRIORITY
            | Ready::AIO
            | Ready::READ_CLOSED
            | Ready::WRITE_CLOSED
            | Ready::ERROR
            | Ready::HANGUP;
        let s = std::format!("{:?}", all);
        assert!(s.contains("READABLE"));
        assert!(s.contains("WRITABLE"));
        assert!(s.contains("PRIORITY"));
        assert!(s.contains("AIO"));
        assert!(s.contains("READ_CLOSED"));
        assert!(s.contains("WRITE_CLOSED"));
        assert!(s.contains("ERROR"));
        assert!(s.contains("HANGUP"));
        // Joined by " | "
        assert_eq!(s.matches(" | ").count(), 7);

        let s = std::format!("{:?}", Ready::empty());
        assert_eq!(s, "(empty)");
    }

    // ----- IoError + IoErrorKind -----

    #[test]
    fn ioerror_new_kind_raw_code() {
        let e = IoError::new(IoErrorKind::NotFound, 2);
        assert_eq!(e.kind(), IoErrorKind::NotFound);
        assert_eq!(e.raw_os_code(), Some(2));
    }

    #[test]
    fn ioerror_zero_code_means_none() {
        let e = IoError::new(IoErrorKind::Other, 0);
        assert_eq!(e.raw_os_code(), None);
    }

    #[test]
    fn ioerror_display_with_and_without_code() {
        let e = IoError::new(IoErrorKind::AlreadyRegistered, 17);
        let s = std::format!("{}", e);
        assert!(s.contains("AlreadyRegistered"));
        assert!(s.contains("os error 17"));

        let e = IoError::new(IoErrorKind::BackendUnavailable, 0);
        let s = std::format!("{}", e);
        assert!(s.contains("BackendUnavailable"));
        assert!(!s.contains("os error"));
    }

    #[test]
    fn ioerror_eq_clone_copy() {
        let a = IoError::new(IoErrorKind::Interrupted, 4);
        let b = a; // Copy
        assert_eq!(a, b);
        // Clone returns the same value; call via the trait so clippy
        // doesn't flag the explicit `.clone()` on a Copy type.
        let c = Clone::clone(&a);
        assert_eq!(a, c);
        let d = IoError::new(IoErrorKind::Interrupted, 9);
        assert_ne!(a, d);
    }

    #[test]
    fn ioerrorkind_variants_exhaustive_construct() {
        // Walk every variant — surfaces any future #[non_exhaustive]
        // additions in subsequent test maintenance.
        let kinds = [
            IoErrorKind::AlreadyRegistered,
            IoErrorKind::NotFound,
            IoErrorKind::InvalidInput,
            IoErrorKind::Interrupted,
            IoErrorKind::WouldBlock,
            IoErrorKind::PermissionDenied,
            IoErrorKind::OutOfMemory,
            IoErrorKind::BackendUnavailable,
            IoErrorKind::Other,
        ];
        for k in &kinds {
            let e = IoError::new(*k, 1);
            assert_eq!(e.kind(), *k);
        }
    }

    // ----- IoResult -----

    #[test]
    fn ioresult_ok_and_err_paths() {
        fn ok_path(x: u32) -> IoResult<u32> {
            Ok(x)
        }
        let ok = ok_path(7);
        assert_eq!(ok.expect("Ok branch"), 7);
        let err: IoResult<u32> = Err(IoError::new(IoErrorKind::Other, 0));
        assert!(err.is_err());
    }
}
