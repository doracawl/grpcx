//! Compile-time size assertions on the public ADTs.
//!
//! Each `const _: () = …` block enforces a hard upper bound
//! on a type's `size_of`. If a future change pushes a type
//! over its bound, the build fails — same shape as L7's
//! `grpcx-idl/src/sizes.rs` and L8's `grpcx-pb/src/sizes.rs`.
//!
//! `report_internal_sizes` (test-only) prints a MEM-tier
//! table that gets copied verbatim into `BUDGETS.md`.

use crate::{
    CloseCode, ExtensionOffer, Frame, Handshake, Message, MessageKind, Opcode,
    PermessageDeflateOffer, Reassembler, ReassemblyOutput, WsError,
};

// ============================================================
// Compile-time bounds.
// ============================================================

const _: () = assert!(
    core::mem::size_of::<Opcode>() <= 4,
    "Opcode must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<MessageKind>() <= 4,
    "MessageKind must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<CloseCode>() <= 4,
    "CloseCode must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<WsError>() <= 32,
    "WsError must fit in 32 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Frame>() <= 64,
    "Frame must fit in 64 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Message>() <= 32,
    "Message must fit in 32 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Handshake>() <= 128,
    "Handshake must fit in 128 bytes",
);
const _: () = assert!(
    core::mem::size_of::<ReassemblyOutput>() <= 96,
    "ReassemblyOutput must fit in 96 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Reassembler>() <= 64,
    "Reassembler must fit in 64 bytes",
);
const _: () = assert!(
    core::mem::size_of::<ExtensionOffer>() <= 64,
    "ExtensionOffer must fit in 64 bytes",
);
const _: () = assert!(
    core::mem::size_of::<PermessageDeflateOffer>() <= 8,
    "PermessageDeflateOffer must fit in 8 bytes",
);

// ============================================================
// MEM-tier report (test-only).
// ============================================================

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use std::println;

    /// Emits a MEM-tier table to stdout. Copy verbatim into
    /// `BUDGETS.md`. Run with
    /// `cargo test -p grpcx-ws report_internal_sizes -- --nocapture`.
    #[test]
    fn report_internal_sizes() {
        let rows: &[(&str, usize)] = &[
            ("Opcode", core::mem::size_of::<Opcode>()),
            ("MessageKind", core::mem::size_of::<MessageKind>()),
            ("CloseCode", core::mem::size_of::<CloseCode>()),
            ("WsError", core::mem::size_of::<WsError>()),
            ("Frame", core::mem::size_of::<Frame>()),
            ("Message", core::mem::size_of::<Message>()),
            ("Handshake", core::mem::size_of::<Handshake>()),
            ("ReassemblyOutput", core::mem::size_of::<ReassemblyOutput>()),
            ("Reassembler", core::mem::size_of::<Reassembler>()),
            ("ExtensionOffer", core::mem::size_of::<ExtensionOffer>()),
            (
                "PermessageDeflateOffer",
                core::mem::size_of::<PermessageDeflateOffer>(),
            ),
        ];
        let bounds: &[(&str, usize)] = &[
            ("Opcode", 4),
            ("MessageKind", 4),
            ("CloseCode", 4),
            ("WsError", 32),
            ("Frame", 64),
            ("Message", 32),
            ("Handshake", 128),
            ("ReassemblyOutput", 96),
            ("Reassembler", 64),
            ("ExtensionOffer", 64),
            ("PermessageDeflateOffer", 8),
        ];
        println!("\n{:-^56}", " grpcx-ws MEM ");
        println!("{:<24} | {:>5} | {:>5}", "Type", "Bytes", "Bound");
        println!("{:-<56}", "");
        for ((name, size), (_, bound)) in rows.iter().zip(bounds.iter()) {
            println!("{name:<24} | {size:>5} | {bound:>5}");
        }
    }
}
