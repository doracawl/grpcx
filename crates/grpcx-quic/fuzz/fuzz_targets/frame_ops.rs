//! Fuzz `decode_frame` followed by `encode_frame`.
//!
//! **Invariant**: no input must produce a panic. Decoding may fail
//! (BadFrame / BadPacket), and re-encoding the decoded value may
//! also fail — but neither operation may panic on adversarial bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_quic::__fuzz_helpers::drive_frame(data);
});
