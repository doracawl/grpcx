//! `size_of` snapshots — guard against the MEM-BUDGETS targets
//! quietly drifting. Both static `const _: () = assert!(…)` (so a
//! breakage fails the build) and a runtime report test (so the
//! numbers land in stdout for `BUDGETS.md` updates).
//!
//! Numbers are platform-dependent (alignment + pointer width); the
//! `const_assert` upper bounds give slack to match an aarch64-darwin
//! (16-byte align, 8-byte pointers) build. Tighter platform-specific
//! numbers can land in v1.x.

use crate::{
    ConnectionId, EcnCounts, Error, Frame, LongHeader, PacketHeader, ShortHeader, StreamId,
};
use core::mem::size_of;

// ============================================================
// Compile-time bounds — guard against silent bloat.
// ============================================================

const _: () = assert!(size_of::<ConnectionId>() <= 24);
const _: () = assert!(size_of::<StreamId>() == 8);
const _: () = assert!(size_of::<Error>() <= 8);
const _: () = assert!(size_of::<EcnCounts>() == 24);
const _: () = assert!(size_of::<LongHeader>() <= 128);
const _: () = assert!(size_of::<ShortHeader>() <= 32);
const _: () = assert!(size_of::<PacketHeader>() <= 144);
const _: () = assert!(size_of::<Frame>() <= 128);

/// Report the byte-size of L4's public ADTs. The test harness
/// prints these so `BUDGETS.md`'s MEM column can be filled in.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn report_internal_sizes() {
    extern crate std;
    use std::println;
    println!("grpcx-quic MEM (bytes):");
    println!("  ConnectionId  = {}", size_of::<ConnectionId>());
    println!("  StreamId      = {}", size_of::<StreamId>());
    println!("  Error         = {}", size_of::<Error>());
    println!("  EcnCounts     = {}", size_of::<EcnCounts>());
    println!("  LongHeader    = {}", size_of::<LongHeader>());
    println!("  ShortHeader   = {}", size_of::<ShortHeader>());
    println!("  PacketHeader  = {}", size_of::<PacketHeader>());
    println!("  Frame         = {}", size_of::<Frame>());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_sizes() {
        report_internal_sizes();
    }

    #[test]
    fn connection_id_fits_a_cacheline() {
        // CIDs are passed by value everywhere; if this grows past
        // 24 bytes the const_assert would have fired, but a runtime
        // check keeps the intent visible.
        assert!(size_of::<ConnectionId>() <= 24);
    }
}
