#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![no_std]

//! Layout: the public surface is the [`import`] entry point
//! plus a small set of ADTs that model a parsed `.proto`
//! editions 2023+ schema ([`Schema`] / [`Message`] / [`Field`]
//! / [`Enum`] / [`ImportError`]). v0.1 ships the **shape** of
//! every public item with `unimplemented!()` bodies; each v0.x
//! checkpoint replaces a slice with real code.
//!
//! Design intent (per `../../REFACTOR-v8-v0.1-bootstrap.md`):
//!
//! 1. **Editions-2023-shaped kernel.** The in-memory model is
//!    the editions grammar; `syntax = "proto3"` (stage A, 1.3)
//!    and `syntax = "proto2"` (stage C, 1.5) sources are
//!    translated at the frontend — see [`Dialect`] for the
//!    per-dialect translation notes.
//! 2. **Single-file, no implicit IO.** [`import`] takes raw
//!    `.proto` source; multi-file resolution
//!    (`import "other.proto";`) records imports in
//!    [`Schema::imports`] and the caller drives file IO.
//! 3. **Codegen is text.** The v0.5 codegen entry point emits
//!    a `String` of L7 IDL Rust source; the caller decides
//!    whether to feed it to a `build.rs`, a proc-macro, or a
//!    one-shot codegen binary. Keeps L8 free of
//!    `proc-macro2` / `syn` / `quote` deps.
//! 4. `no_std + alloc` — works in firmware codegen contexts.
//!    No `std` leakage in public API.
//! 5. No external runtime deps. Same charter as L1-L7.
//! 6. Validation errors carry line / column when known
//!    ([`ImportError::Lex`] / [`ImportError::Parse`]) or a
//!    `'static str` reason when not ([`ImportError::Semantic`]
//!    / [`ImportError::UnsupportedFeature`]).

/// Re-exported for generated code: emitted sources
/// reference `::grpcx_pb::alloc::…` so they compile in
/// both `no_std` and `std` consumer crates without an
/// `extern crate alloc;` at the call site.
#[doc(hidden)]
pub extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

mod codegen;
mod lexer;
mod parser;
mod semantic;
mod sizes;
mod wire;

pub use codegen::codegen;
pub use wire::{
    ProtoCodec, WireError, WireType, decode_fixed32, decode_fixed64, decode_length_delimited,
    decode_string, decode_tag, decode_varint, encode_fixed32, encode_fixed64,
    encode_length_delimited, encode_tag, encode_varint, from_proto_bytes, skip_field,
    to_proto_bytes, zigzag_decode_i32, zigzag_decode_i64, zigzag_encode_i32, zigzag_encode_i64,
};

// ============================================================
// Entry point.
// ============================================================

/// Parse a `.proto` editions 2023+ source string into a
/// [`Schema`].
///
/// The full pipeline runs end to end: lexer → parser →
/// semantic analyser. Errors short-circuit at the first
/// failing stage.
///
/// `import` does not read files — `import "other.proto";`
/// statements are recorded in [`Schema::imports`] and the
/// caller is responsible for resolving them (typically by
/// reading the file and recursing into another `import` call).
///
/// # Errors
///
/// - [`ImportError::Lex`] — token-level rejection
///   (unrecognised character, invalid string escape,
///   `syntax = "proto2"` / `syntax = "proto3"` at the syntax
///   line, etc.).
/// - [`ImportError::Parse`] — grammar-level rejection
///   (missing semicolon, unbalanced braces, unknown field
///   label, etc.).
/// - [`ImportError::Semantic`] — analyser rejection (duplicate
///   field number, unresolved type reference, field number
///   in reserved range, etc.).
/// - [`ImportError::UnsupportedFeature`] — editions feature
///   not yet implemented (oneof-with-options edge cases on
///   v0.x, etc.).
///
/// # v0.x implementation status
///
/// v0.4 adds the semantic analyser. `import` now runs lexer,
/// syntax-line gate, parser, and semantic analyser in
/// sequence. The returned [`Schema`] has all named type
/// references resolved to fully-qualified names, with
/// `FieldType::Message` vs `FieldType::Enum` correctly
/// distinguished. Field numbers are range-checked
/// (1..=536_870_911 minus 19000..=19999 reserved); duplicate
/// field names, field numbers, and enum value names within a
/// single declaration are rejected. Codegen lands at v0.5;
/// runtime wire-format shim at v0.6.
pub fn import(source: &str) -> Result<Schema, ImportError> {
    let (tokens, dialect) = lexer::tokenize(source)?;
    let mut schema = parser::parse(&tokens, dialect)?;
    semantic::analyse(&mut schema)?;
    Ok(schema)
}

// ============================================================
// Schema root.
// ============================================================

/// Parsed `.proto` schema — root of the L8 in-memory model.
///
/// Populated by [`import`]; consumed by the v0.5 codegen + v0.6
/// runtime wire-format shim.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Schema {
    /// `edition` declaration from the syntax line.
    pub edition: Edition,
    /// `package foo.bar;` if present; the dotted path is kept
    /// as-is in the string.
    pub package: Option<String>,
    /// `import "other.proto";` filenames in source order;
    /// caller drives multi-file resolution.
    pub imports: Vec<String>,
    /// Top-level `message Foo { ... }` declarations.
    pub messages: Vec<Message>,
    /// Top-level `enum Foo { ... }` declarations.
    pub enums: Vec<Enum>,
    /// Top-level `service Foo { ... }` declarations
    /// (v1.1+; previously parse-and-discarded). Consumed
    /// by the typed-stub codegen.
    pub services: Vec<ServiceDef>,
    /// The source dialect this schema was translated from
    /// (1.3+). The model itself is always 2023-shaped.
    pub source_dialect: Dialect,
}

/// A `service Foo { rpc … }` declaration.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServiceDef {
    /// Service name as written (unqualified; combine with
    /// [`Schema::package`] for the wire-level
    /// `/package.Service/Method` path).
    pub name: String,
    /// The `rpc` methods in source order.
    pub methods: Vec<MethodDef>,
}

/// One `rpc Name (In) returns (Out);` declaration.
///
/// Input/output type names are kept as written (possibly
/// dotted). Existence is enforced by the generated code's
/// compilation rather than at import time — the codegen
/// references the types, so a typo fails `rustc` with a
/// normal unresolved-type error.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MethodDef {
    /// Method name as written (PascalCase per style guide).
    pub name: String,
    /// Request message type name.
    pub input: String,
    /// Response message type name.
    pub output: String,
    /// `stream` on the request side.
    pub client_streaming: bool,
    /// `stream` on the response side.
    pub server_streaming: bool,
}

/// Source dialect the schema was written in. The in-memory
/// model is ALWAYS editions-2023 shaped — proto3 sources
/// are translated at the frontend (the official editions
/// design: proto2/proto3 behaviours are expressible as
/// edition-feature values, and all three share one wire
/// format). Recorded for diagnostics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `edition = "2023";`
    #[default]
    Edition2023,
    /// `syntax = "proto3";` — accepted since 1.3 (DX
    /// stage A). Translation notes: the `optional` field
    /// prefix maps to [`Label::Optional`] (the stage-B
    /// presence kernel, 1.4): generated fields are
    /// `Option<T>` with explicit presence on the wire.
    /// First enum value must be 0.
    Proto3,
    /// `syntax = "proto2";` — accepted since 1.5 (DX
    /// stage C). Every regular message field carries
    /// exactly one label: `optional` maps to
    /// [`Label::Optional`] (explicit presence,
    /// `Option<T>` — the stage-B kernel), `required` maps
    /// to [`Label::Singular`] (editions `LEGACY_REQUIRED`
    /// always-serializes, and so does our Singular
    /// encode; missing-required decode is NOT enforced —
    /// prost doesn't either). `map<…>` fields take no
    /// label; `[default = …]` options are parsed and
    /// discarded (fields are `Option<T>`, no
    /// default-returning getters); `group` / `extend` /
    /// `extensions` are rejected.
    Proto2,
}

/// Supported `.proto` edition.
///
/// v0.1 / v8.0 ships `Edition2023` only. Future editions land
/// here additively as the protobuf project publishes them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// `edition = "2023";` — the v8.0 reference grammar.
    #[default]
    Edition2023,
}

// ============================================================
// Messages, fields, types.
// ============================================================

/// A `message Foo { ... }` declaration.
///
/// `nested_messages` / `nested_enums` capture `.proto`'s
/// arbitrary nesting depth — they are not flattened into the
/// parent [`Schema`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    /// Simple name from `message <name> { ... }`. Fully-
    /// qualified name (with package + outer scopes) is the
    /// caller's responsibility.
    pub name: String,
    /// Field declarations in source order.
    pub fields: Vec<Field>,
    /// `message Bar { ... }` declared inside this message.
    pub nested_messages: Vec<Message>,
    /// `enum Bar { ... }` declared inside this message.
    pub nested_enums: Vec<Enum>,
}

/// A field within a [`Message`].
///
/// Field numbers are validated against editions 2023 rules
/// (1..=536870911, excluding the 19000-19999 reserved range)
/// at the semantic-analysis stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field name as written in the `.proto`.
    pub name: String,
    /// Wire tag number (1..=536870911 excluding reserved).
    pub number: u32,
    /// Field type — primitive, message reference, enum
    /// reference, or map.
    pub ty: FieldType,
    /// Field label.
    pub label: Label,
}

/// Field label.
///
/// Editions 2023+ drops the proto2 `Required`; `Optional`
/// collapses into [`Singular`](Self::Singular). The remaining
/// distinction is between scalar / single-message
/// ([`Singular`](Self::Singular)) and `repeated`
/// ([`Repeated`](Self::Repeated)) fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Single-occurrence field (implicit presence:
    /// defaults stay off the wire).
    Singular,
    /// proto3 `optional` (stage B): explicit presence —
    /// the generated field is `Option<T>`; `Some(default)`
    /// goes ON the wire, `None` doesn't.
    Optional,
    /// `repeated` field.
    Repeated,
}

/// Field type — scalar, named-type reference, or map.
///
/// Scalar variants mirror the `.proto` scalar type keywords.
/// `String_` carries a trailing underscore to avoid shadowing
/// Rust's `String`. `Message(name)` and `Enum(name)` hold the
/// referenced type's name as written in the `.proto`;
/// resolution happens at the semantic-analysis stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    /// `int32` — variable-width signed, varint on wire.
    Int32,
    /// `int64` — variable-width signed, varint on wire.
    Int64,
    /// `uint32` — variable-width unsigned, varint on wire.
    Uint32,
    /// `uint64` — variable-width unsigned, varint on wire.
    Uint64,
    /// `sint32` — zigzag-encoded signed, varint on wire.
    Sint32,
    /// `sint64` — zigzag-encoded signed, varint on wire.
    Sint64,
    /// `fixed32` — fixed 4-byte unsigned, LE on wire.
    Fixed32,
    /// `fixed64` — fixed 8-byte unsigned, LE on wire.
    Fixed64,
    /// `sfixed32` — fixed 4-byte signed, LE on wire.
    Sfixed32,
    /// `sfixed64` — fixed 8-byte signed, LE on wire.
    Sfixed64,
    /// `float` — IEEE 754 single, LE on wire.
    Float,
    /// `double` — IEEE 754 double, LE on wire.
    Double,
    /// `bool` — single byte on wire (0 / 1).
    Bool,
    /// `string` — length-prefixed UTF-8.
    ///
    /// Named `String_` (with trailing underscore) to avoid
    /// shadowing `alloc::string::String`.
    String_,
    /// `bytes` — length-prefixed opaque byte payload.
    Bytes,
    /// Reference to a `message` type by **fully-qualified
    /// name** (post-v0.4 semantic analysis).
    ///
    /// The carried string is the FQN
    /// (`<package>.<scope>.<name>`-style; no leading `.`).
    /// The v0.3 parser emits this variant for **all** named
    /// type references with the user-written (possibly
    /// unqualified) name; v0.4's semantic analyser rewrites
    /// the name to its FQN and reclassifies refs that
    /// resolve to enum declarations over to
    /// [`FieldType::Enum`].
    Message(String),
    /// Reference to an `enum` type by **fully-qualified
    /// name** (post-v0.4 semantic analysis).
    ///
    /// Populated only by v0.4's reclassify step; the v0.3
    /// parser never emits this variant directly. See
    /// [`FieldType::Message`].
    Enum(String),
    /// `map<K, V>` — `K` is one of the scalar types other than
    /// `Float`/`Double`/`Bytes`; `V` is any [`FieldType`].
    /// Validation happens at the semantic-analysis stage.
    Map {
        /// Key type — boxed because [`FieldType`] is
        /// recursive through this variant.
        key: Box<FieldType>,
        /// Value type — boxed for the same reason.
        value: Box<FieldType>,
    },
}

// ============================================================
// Enums.
// ============================================================

/// An `enum Foo { ... }` declaration.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Enum {
    /// Simple name from `enum <name> { ... }`.
    pub name: String,
    /// Value declarations in source order.
    pub values: Vec<EnumValue>,
}

/// A single value within an [`Enum`].
///
/// Enum values in `.proto` are 32-bit **signed** (per the spec
/// — `int32` on the wire), so [`number`](Self::number) is
/// `i32`, not `u32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    /// Value name as written in the `.proto`.
    pub name: String,
    /// Numeric value (signed; editions 2023+ inherits proto3's
    /// requirement that the first value is 0 for "open" enums,
    /// validated at semantic-analysis stage).
    pub number: i32,
}

// ============================================================
// Errors.
// ============================================================

/// Failure modes for [`import`].
///
/// Lexer / parser errors carry line + column for callers that
/// want to point at the offending source location; semantic /
/// unsupported-feature errors carry a `'static str` reason
/// because the failing location is typically a span rather
/// than a single point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// Lexer rejection — unrecognised character, invalid
    /// string escape, a malformed syntax/edition line, etc.
    Lex {
        /// 1-based line number.
        line: u32,
        /// 1-based column number.
        col: u32,
    },
    /// Parser rejection — missing semicolon, unbalanced
    /// braces, unknown field label, etc.
    Parse {
        /// 1-based line number.
        line: u32,
        /// 1-based column number.
        col: u32,
    },
    /// Semantic analyser rejection — duplicate field number,
    /// unresolved type reference, field number in reserved
    /// range, etc.
    Semantic(&'static str),
    /// Editions feature not yet implemented (oneof-with-
    /// options edge cases on v0.x, etc.).
    UnsupportedFeature(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn error_is_compact() {
        // `ImportError` carries at most `(u32, u32)` (Lex /
        // Parse arms) or a `&'static str` (Semantic /
        // UnsupportedFeature). On 64-bit, the fat pointer is
        // 16 bytes + 4 bytes discriminant + padding ≤ 24
        // bytes total. The 32-byte bound matches the L7
        // `Error` bound for consistency across layers.
        assert!(size_of::<ImportError>() <= 32);
    }

    #[test]
    fn default_schema_is_empty() {
        // Smoke: `Schema::default()` constructs without
        // allocation in the common path (empty `Vec` is
        // pointer-sized; no heap touch) and matches a hand-
        // written empty `Schema`.
        let s = Schema::default();
        assert_eq!(s.edition, Edition::Edition2023);
        assert!(s.package.is_none());
        assert!(s.imports.is_empty());
        assert!(s.messages.is_empty());
        assert!(s.enums.is_empty());
    }
}
