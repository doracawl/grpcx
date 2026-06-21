//! Internal-RPC wire format v2 codec.
//!
//! Normative reference: `_dev/SPEC-wire-v2.md`.
//!
//! The v2 grammar is the protocol shared by the
//! internal-RPC transports (`transport-shmem`,
//! `transport-uds`, `transport-tcp-mux` v2 mode). It is
//! disjoint from the v1 mux frames used by the existing
//! `transport-tcp-mux` (`crate::mux`) and from the
//! HTTP/2 + HTTP/3 framings used by the interop /
//! cross-host transports.
//!
//! ## Module layout
//!
//! - [`frame`] — pure value types ([`Header`],
//!   [`FrameType`], [`Flags`], `HEADER_LEN`).
//! - [`metadata`] — [`MetadataEntry`] + encode/decode
//!   of the metadata block.
//! - [`codec`] — fixed 16-byte header encode/decode.
//! - [`errors`] — [`WireError`] enum.
//!
//! The crate-level prelude re-exports the symbols an
//! application or another transport crate is expected
//! to touch; reach into the submodules only for
//! internals.

pub mod codec;
pub mod errors;
pub mod frame;
pub mod metadata;
pub mod pseudo;

pub use codec::{decode_header, decode_header_from_slice, encode_header};
pub use errors::WireError;
pub use frame::{Flags, FrameType, HEADER_LEN, Header};
pub use metadata::{
    MetadataEntry, NAME_LEN_MAX, block_wire_len, decode_metadata_block, encode_metadata_block,
    entry_wire_len,
};
pub use pseudo::{
    BEGIN_PSEUDO_COUNT, STATUS_OK_BLOCK, STATUS_OK_COUNT, begin_block_len, encode_begin_block,
};
