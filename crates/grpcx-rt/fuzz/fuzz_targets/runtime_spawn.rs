//! Fuzz a single-thread `block_on` spawn-storm over a shared channel.
//!
//! Input bytes parameterize `N ≤ 8` producers × `per ≤ 32` messages
//! through a `cap ≤ 16` ring buffer; the driver asserts the receiver's
//! closed-form sum + count match the producers' wrapping arithmetic.
//! Exercises the slab pool, the wake state-machine, the channel `recv`
//! await path, and `yield_now`'s leaf-dispatch fallback (no MT runtime
//! → current-thread path). The fuzzer must never observe a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_rt::__fuzz_helpers::drive_runtime(data);
});
