#![doc = include_str!("../README.md")]
#![no_std]

//! Layout (mirrors L5):
//! - [`hpack`] — RFC 7541 header compression: static +
//!   dynamic tables, prefixed integers, Huffman decode
//!   (encode side emits literals — legal and simple).
//! - `frame` (next checkpoint) — RFC 9113 frame codec.
//! - `connection` (next) — stream state machine +
//!   two-level flow control, sans-IO byte pump.

extern crate alloc;

pub mod connection;
pub mod frame;
pub mod hpack;

pub use connection::{Connection, Header, RequestStream};
pub use frame::H2Error;
