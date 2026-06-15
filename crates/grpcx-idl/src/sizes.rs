//! `size_of` snapshots — guards against the MEM budgets in
//! `BUDGETS.md` quietly drifting.
//!
//! Numbers are platform-dependent (alignment + pointer width);
//! the `const_assert` upper bounds give slack to match an
//! aarch64-darwin (16-byte align, 8-byte pointers) build.

use crate::{
    ArchivedOption, ArchivedResult, ArchivedTuple2, ArchivedTuple3, ArchivedTuple4, Error,
};
use core::mem::size_of;

// Compile-time bounds.
// Error carries a `Custom(&'static str)` variant — 16 bytes
// for the slice + 8 bytes disc/align = 24 in practice. 32 cap
// preserves room for one more pointer-sized variant if added.
const _: () = assert!(size_of::<Error>() <= 32);
const _: () = assert!(size_of::<ArchivedTuple2<u32, u32>>() == 8);
const _: () = assert!(size_of::<ArchivedTuple3<u32, u16, u8>>() == 8);
const _: () = assert!(size_of::<ArchivedTuple4<u8, u16, u32, u64>>() == 16);
const _: () = assert!(size_of::<ArchivedOption<u32>>() == 8);
const _: () = assert!(size_of::<ArchivedOption<u64>>() == 16);
const _: () = assert!(size_of::<ArchivedResult<u32, u32>>() == 8);

/// Print the public ADTs' byte sizes. Tests call this so
/// `BUDGETS.md`'s MEM column can be filled.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn report_internal_sizes() {
    extern crate std;
    use std::println;
    println!("grpcx-idl MEM (bytes):");
    println!("  Error                          = {}", size_of::<Error>());
    println!(
        "  ArchivedTuple2<u32, u32>       = {}",
        size_of::<ArchivedTuple2<u32, u32>>()
    );
    println!(
        "  ArchivedTuple3<u32, u16, u8>   = {}",
        size_of::<ArchivedTuple3<u32, u16, u8>>()
    );
    println!(
        "  ArchivedTuple4<u8, u16, u32, u64> = {}",
        size_of::<ArchivedTuple4<u8, u16, u32, u64>>()
    );
    println!(
        "  ArchivedOption<u32>            = {}",
        size_of::<ArchivedOption<u32>>()
    );
    println!(
        "  ArchivedOption<u64>            = {}",
        size_of::<ArchivedOption<u64>>()
    );
    println!(
        "  ArchivedResult<u32, u32>       = {}",
        size_of::<ArchivedResult<u32, u32>>()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_sizes() {
        report_internal_sizes();
    }

    #[test]
    fn error_fits_a_cacheline() {
        assert!(size_of::<Error>() <= 32);
    }
}
