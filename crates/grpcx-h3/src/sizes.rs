//! `size_of` snapshots — guard against the MEM budgets in
//! `BUDGETS.md` quietly drifting. Both `const _: () = assert!(…)`
//! (so a breakage fails the build) and a runtime report test
//! (so the numbers land in stdout for budget updates).
//!
//! Numbers are platform-dependent (alignment + pointer width);
//! the `const_assert` upper bounds give slack to match an
//! aarch64-darwin (16-byte align, 8-byte pointers) build.

use crate::{DynamicEntry, EncoderInstruction, Frame, Header, Settings, StreamType};
use core::mem::size_of;

const _: () = assert!(size_of::<Header>() <= 64);
const _: () = assert!(size_of::<Settings>() <= 64);
const _: () = assert!(size_of::<StreamType>() <= 24);
const _: () = assert!(size_of::<Frame>() <= 64);
const _: () = assert!(size_of::<EncoderInstruction>() <= 96);
const _: () = assert!(size_of::<DynamicEntry>() <= 64);

/// Print the public ADTs' byte sizes. The test harness calls
/// this so `BUDGETS.md`'s MEM column can be filled in.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn report_internal_sizes() {
    extern crate std;
    use std::println;
    println!("grpcx-h3 MEM (bytes):");
    println!("  Header              = {}", size_of::<Header>());
    println!("  Settings            = {}", size_of::<Settings>());
    println!("  StreamType          = {}", size_of::<StreamType>());
    println!("  Frame               = {}", size_of::<Frame>());
    println!(
        "  EncoderInstruction  = {}",
        size_of::<EncoderInstruction>()
    );
    println!("  DynamicEntry        = {}", size_of::<DynamicEntry>());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_sizes() {
        report_internal_sizes();
    }

    #[test]
    fn header_fits_a_cacheline() {
        assert!(size_of::<Header>() <= 64);
    }
}
