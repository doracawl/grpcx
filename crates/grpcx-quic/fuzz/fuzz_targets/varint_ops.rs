//! Fuzz `frame_varint_len` / `varint_field_len`.
//!
//! **Invariant**: no input must produce a panic. Both helpers are
//! pure classifiers and must always return one of {1, 2, 4, 8}.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_quic::__fuzz_helpers::drive_varint(data);
});
