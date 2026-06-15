//! Source trait — implemented by every IO source registerable with
//! a reactor.

use crate::reactor::Registry;
use crate::types::{Interest, IoResult, Token};

/// Common trait implemented by every IO source registerable with a
/// reactor.
///
/// # Implementor responsibilities
///
/// Implementors hand the reactor an OS-level handle (raw fd, raw
/// socket, raw `HANDLE`) for the duration of registration. The
/// handle must remain valid until either:
///
/// - [`deregister`](Self::deregister) is called, or
/// - the owning [`Poll`](crate::Poll) is dropped.
///
/// Violating this contract corrupts the reactor's bookkeeping at the
/// OS layer (e.g. `epoll` holds the fd's epitem until
/// `EPOLL_CTL_DEL`). The trait is **not** marked `unsafe` because the
/// validity guarantee is a documented contract, not a memory-safety
/// invariant under the abstract machine; broken contracts surface as
/// OS-level errors, not UB.
///
/// # Why three explicit methods
///
/// `epoll` uses `EPOLL_CTL_{ADD,MOD,DEL}` — three distinct syscalls.
/// `kqueue` uses one `kevent` call with `EV_{ADD,DELETE}` flags.
/// `IOCP` associates once and re-association is sticky. Hiding the
/// verb at the `Source` layer (rather than re-deriving from "is this
/// a re-register?") lets each backend route correctly without extra
/// state lookups.
pub trait Source {
    /// Register this source with `registry` under `token`, requesting
    /// notifications for `interest`.
    fn register(&mut self, registry: &Registry, token: Token, interest: Interest) -> IoResult<()>;

    /// Update an already-registered source's token and interest set.
    fn reregister(&mut self, registry: &Registry, token: Token, interest: Interest)
    -> IoResult<()>;

    /// Remove this source from `registry`.
    fn deregister(&mut self, registry: &Registry) -> IoResult<()>;
}
