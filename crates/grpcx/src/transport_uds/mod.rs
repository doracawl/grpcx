//! `transport-uds` — AF_UNIX same-host transport carrying
//! wire-format v2 frames.
//!
//! Public types: [`UdsConnection`] (client side) and
//! [`UdsServerConn`] (server-side accepted connection).
//! Server-side serve loops are entered via
//! `Server::listen_uds` / `Server::listen_uds_until`
//! (see [`crate::Server`]).
//!
//! See `_dev/RFC-internal-rpc-arch.md` § 4 for the
//! design rationale and `_dev/SPEC-wire-v2.md` for the
//! normative wire grammar.

pub(crate) mod connection;
pub(crate) mod peer;
pub(crate) mod server;
pub(crate) mod wire;

pub use connection::UdsConnection;
pub use server::UdsServerConn;
