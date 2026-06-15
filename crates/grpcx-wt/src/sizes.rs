//! `size_of` snapshots — guard against the MEM budgets in
//! `BUDGETS.md` drifting silently. The `const _: () = assert!(…)`
//! lines fail the build if any of the public ADTs grow past
//! their upper bound; the runtime `report_internal_sizes`
//! prints the current numbers for budget updates.
//!
//! Numbers are platform-dependent (alignment + pointer width);
//! upper bounds give slack to match an aarch64-darwin build
//! (16-byte alignment, 8-byte pointers).

use crate::{BidirectionalStream, Capsule, Error, Session, SessionMux, UnidirectionalStream};
use core::mem::size_of;

const _: () = assert!(size_of::<Session>() <= 512);
const _: () = assert!(size_of::<SessionMux>() <= 128);
const _: () = assert!(size_of::<BidirectionalStream>() <= 16);
const _: () = assert!(size_of::<UnidirectionalStream>() <= 16);
const _: () = assert!(size_of::<Capsule>() <= 96);
const _: () = assert!(size_of::<Error>() <= 64);

/// Print the public ADTs' byte sizes. The test harness calls
/// this so `BUDGETS.md`'s MEM column can be filled in.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn report_internal_sizes() {
    extern crate std;
    use std::println;
    println!("grpcx-wt MEM (bytes):");
    println!("  Session              = {}", size_of::<Session>());
    println!("  SessionMux           = {}", size_of::<SessionMux>());
    println!(
        "  BidirectionalStream  = {}",
        size_of::<BidirectionalStream>()
    );
    println!(
        "  UnidirectionalStream = {}",
        size_of::<UnidirectionalStream>()
    );
    println!("  Capsule              = {}", size_of::<Capsule>());
    println!("  Error                = {}", size_of::<Error>());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_sizes() {
        report_internal_sizes();
    }

    #[test]
    fn stream_handles_are_minimal() {
        assert_eq!(size_of::<BidirectionalStream>(), size_of::<u64>());
        assert_eq!(size_of::<UnidirectionalStream>(), size_of::<u64>());
    }
}
