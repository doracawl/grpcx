//! Fuzz the bounded `mpsc` channel state machine.
//!
//! Input bytes script a single-threaded sequence of try_send / try_recv /
//! clone-sender / drop-sender / drop-receiver ops; the driver checks
//! conservation (every value try_send'd exactly once must come back
//! exactly once, or be observable when the receiver drops) and the
//! Closed / Disconnected transitions. The fuzzer must never observe a
//! panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_rt::__fuzz_helpers::drive_channel(data);
});
