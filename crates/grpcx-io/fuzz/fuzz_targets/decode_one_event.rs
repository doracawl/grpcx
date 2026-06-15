//! Fuzz the per-OS event decoder.
//!
//! Input bytes are interpreted as the kernel's native event struct
//! (`EpollEvent` on Linux, `Kevent` on Darwin/iOS/FreeBSD). The
//! decoder runs through the same translation logic `Events::poll_with`
//! uses; the fuzzer must never observe a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(event) = grpcx_io::__fuzz_helpers::decode_one_event(data) {
        // Trivially read the decoded fields to make sure no later
        // panic surfaces during access.
        let _ = event.token();
        let _ = event.ready();
    }
});
