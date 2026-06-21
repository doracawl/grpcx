//! `transport-shmem` — POSIX shared-memory + UDS
//! rendezvous transport for grpcx internal RPC v2.
//!
//! T3.1 ships the rendezvous control plane:
//! - [`Region`] — POSIX shm region create / open / mmap.
//! - [`WakeFd`] — `pipe(2)`-backed wake fd pair (cross-OS
//!   `eventfd` substitute; Linux can upgrade to a true
//!   `eventfd` in a perf push without changing this API).
//! - [`rendezvous`] — `accept_rendezvous` + `connect_rendezvous`
//!   exchange a [`RegionDescriptor`] over an open UDS
//!   stream, passing wake fds via `SCM_RIGHTS`.
//!
//! The SPSC ringbuf data plane (T3.2) layers on top.
//! v1 supports Linux + Darwin + FreeBSD; Windows is out
//! of scope per `_dev/RFC-internal-rpc-arch.md` § 3.6.

pub mod connection;
pub mod region;
pub mod rendezvous;
pub mod ring;
pub mod server;
pub mod wake;

pub use connection::ShmemConnection;
pub use rendezvous::{RegionDescriptor, ServerSideRegion, accept_rendezvous, connect_rendezvous};
