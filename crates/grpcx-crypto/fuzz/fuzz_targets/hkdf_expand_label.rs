//! Fuzz `Hkdf::expand_label`'s label + context length-prefix
//! wire-format. Input bytes script `(label_len, label, context_len,
//! context)`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_crypto::__fuzz_helpers::drive_hkdf_expand_label(data);
});
