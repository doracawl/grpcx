//! `size_of` snapshots — guard against the MEM budgets in
//! `BUDGETS.md` drifting silently. The `const _: () =
//! assert!(…)` lines fail the build if any public ADT
//! grows past its bound. Bounds match aarch64-darwin
//! (16-byte alignment, 8-byte pointers).

use crate::{MessageFrame, Metadata, Method, RpcError, RpcKind, Status, StatusCode, TransportKind};
use core::mem::size_of;

const _: () = assert!(size_of::<Method>() <= 56);
const _: () = assert!(size_of::<RpcKind>() <= 1);
const _: () = assert!(size_of::<Status>() <= 32);
const _: () = assert!(size_of::<StatusCode>() <= 4);
const _: () = assert!(size_of::<Metadata>() <= 24);
const _: () = assert!(size_of::<RpcError>() <= 96);
const _: () = assert!(size_of::<MessageFrame>() <= 32);
const _: () = assert!(size_of::<TransportKind>() <= 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_sizes_for_budgets_md() {
        // MEM table source for BUDGETS.md (run with
        // `cargo test -p grpcx report_sizes -- --nocapture`).
        extern crate std;
        for (name, size) in [
            ("Method", size_of::<Method>()),
            ("RpcKind", size_of::<RpcKind>()),
            ("Status", size_of::<Status>()),
            ("StatusCode", size_of::<StatusCode>()),
            ("Metadata", size_of::<Metadata>()),
            ("RpcError", size_of::<RpcError>()),
            ("MessageFrame", size_of::<MessageFrame>()),
            ("TransportKind", size_of::<TransportKind>()),
        ] {
            std::println!("MEM {name}: {size} B");
        }
    }
}
