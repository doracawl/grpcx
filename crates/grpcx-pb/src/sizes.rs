//! Compile-time size assertions on the public ADTs.
//!
//! Each `const _: () = …` block enforces a hard upper bound
//! on a type's `size_of`. If a future change pushes a type
//! over its bound, the build fails — same shape as L7's
//! `grpcx-idl/src/sizes.rs`.
//!
//! `report_internal_sizes` (test-only) prints a MEM-tier
//! table that gets copied verbatim into `BUDGETS.md`.

use crate::{Edition, Enum, EnumValue, Field, FieldType, ImportError, Label, Message, Schema};
use crate::{WireError, WireType};

// ============================================================
// Compile-time bounds.
// ============================================================

const _: () = assert!(
    core::mem::size_of::<ImportError>() <= 32,
    "ImportError must fit in 32 bytes",
);
const _: () = assert!(
    core::mem::size_of::<WireError>() <= 32,
    "WireError must fit in 32 bytes",
);
const _: () = assert!(
    core::mem::size_of::<WireType>() <= 4,
    "WireType must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Edition>() <= 4,
    "Edition must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Label>() <= 4,
    "Label must fit in 4 bytes",
);
const _: () = assert!(
    core::mem::size_of::<FieldType>() <= 48,
    "FieldType must fit in 48 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Field>() <= 64,
    "Field must fit in 64 bytes",
);
const _: () = assert!(
    core::mem::size_of::<EnumValue>() <= 32,
    "EnumValue must fit in 32 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Enum>() <= 64,
    "Enum must fit in 64 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Message>() <= 96,
    "Message must fit in 96 bytes",
);
const _: () = assert!(
    core::mem::size_of::<Schema>() <= 128,
    "Schema must fit in 128 bytes",
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
    /// `cargo test -p grpcx-pb report_internal_sizes -- --nocapture`.
    #[test]
    fn report_internal_sizes() {
        let rows: &[(&str, usize)] = &[
            ("ImportError", core::mem::size_of::<ImportError>()),
            ("WireError", core::mem::size_of::<WireError>()),
            ("WireType", core::mem::size_of::<WireType>()),
            ("Edition", core::mem::size_of::<Edition>()),
            ("Label", core::mem::size_of::<Label>()),
            ("FieldType", core::mem::size_of::<FieldType>()),
            ("Field", core::mem::size_of::<Field>()),
            ("EnumValue", core::mem::size_of::<EnumValue>()),
            ("Enum", core::mem::size_of::<Enum>()),
            ("Message", core::mem::size_of::<Message>()),
            ("Schema", core::mem::size_of::<Schema>()),
        ];
        println!("\n{:-^46}", " grpcx-pb MEM ");
        println!("{:<20} | {:>5} | {:>5}", "Type", "Bytes", "Bound");
        println!("{:-<46}", "");
        let bounds: &[(&str, usize)] = &[
            ("ImportError", 32),
            ("WireError", 32),
            ("WireType", 4),
            ("Edition", 4),
            ("Label", 4),
            ("FieldType", 48),
            ("Field", 64),
            ("EnumValue", 32),
            ("Enum", 64),
            ("Message", 96),
            ("Schema", 128),
        ];
        for ((name, size), (_, bound)) in rows.iter().zip(bounds.iter()) {
            println!("{name:<20} | {size:>5} | {bound:>5}");
        }
    }
}
