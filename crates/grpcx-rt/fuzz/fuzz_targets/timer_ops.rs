//! Fuzz the hierarchical timing wheel.
//!
//! Input bytes script insert / advance ops; the driver checks the
//! wheel never fires a timer early and never leaves one stuck. The
//! fuzzer must never observe a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_rt::__fuzz_helpers::drive_timer(data);
});
