//! Fuzz `decode_header` / `encode_header` round-trip.
//!
//! **Invariant**: no input must produce a panic. Parsing must
//! always return either a valid header or `Err(BadPacket)`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_quic::__fuzz_helpers::drive_packet_header(data);
});
