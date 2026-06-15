//! Fuzz the lock-free Chase-Lev work-stealing deque.
//!
//! Input bytes script a single-threaded sequence of push / pop / steal
//! ops; the driver checks conservation (every value removed exactly
//! once) and the `unsafe` move-out discipline. The fuzzer must never
//! observe a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_rt::__fuzz_helpers::drive_deque(data);
});
