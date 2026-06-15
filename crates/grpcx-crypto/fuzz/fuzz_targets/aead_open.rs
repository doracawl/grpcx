//! Fuzz `Aead::open_in_place` across the three suites.
//!
//! Input bytes script a suite pick + nonce + AAD + ciphertext;
//! the driver checks that no input produces a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_crypto::__fuzz_helpers::drive_aead_open(data);
});
