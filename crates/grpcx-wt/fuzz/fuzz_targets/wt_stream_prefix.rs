#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    grpcx_wt::__fuzz_helpers::drive_wt_stream_prefix(data);
});
