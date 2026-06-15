#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    grpcx_h3::__fuzz_helpers::drive_frame(data);
});
