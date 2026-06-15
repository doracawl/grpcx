# Changelog

All notable changes to `grpcx-pb` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.5.0] below)

## [1.5.0] - 2026-06-12

**Stage C — the proto2 frontend.** The last rejected dialect
at the syntax-line gate translates onto the editions-2023
kernel; codegen untouched.

### Added
- `Dialect::Proto2`; `syntax = "proto2";` accepted (was:
  rejected with the stage-C placeholder message).
- proto2 field labels: `optional` → `Label::Optional` (the
  stage-B explicit-presence kernel — `Option<T>`,
  `Some(default)` ON the wire), `required` →
  `Label::Singular` (editions `LEGACY_REQUIRED`
  always-serializes, and so does Singular encode;
  **missing-required decode is NOT enforced** — prost
  doesn't either), `repeated` → `Label::Repeated`.
- Label-less regular proto2 message fields are rejected
  (protoc parity); `map<…>` fields and oneof members stay
  label-less. Double labels and labeled maps are rejected.
- Named rejections: `extensions … ;` message members and
  labeled `group` fields surface as `UnsupportedFeature`
  (were: generic parse errors).
- `[default = …]` field options are parsed and discarded
  (fields are `Option<T>` — no default-returning getters;
  wire bytes unaffected under explicit presence).
- The first-enum-value-zero check stays proto3-only:
  proto2 enums may start at any number (KAT-locked).

### Acceptance
- Lexer/parser/semantic/codegen KATs; compile-level
  `proto_inline!` proto2 wire round trips (unset optional
  off-wire, `Some(default)` on-wire, required always
  encoded); **prost proto2 wire-compat gold** in the
  interop gate — cross-decode both ways + byte-identical
  on all cases (repeated scalars are unpacked in both:
  the proto2 default).

## [1.4.0] - 2026-06-13

**Stage B — the proto3 presence kernel.** The `optional`
field prefix now means what it says.

### Added
- `Label::Optional` (explicit presence): the parser maps
  proto3 `optional` to it (was: silently collapsed to
  `Singular` since stage A); `optional repeated` and
  `optional map<…>` are rejected with
  `UnsupportedFeature`.
- Codegen: optional fields generate `Option<T>` (any
  field type incl. message/enum); encode is Some-gated
  with `Some(default)` going ON the wire (the explicit-
  presence bit) and `None` staying off it; a decoded
  occurrence assigns `Some(v)`; `Default` yields `None`.

### Acceptance
- Parser/codegen KATs; compile-level `proto_inline!` wire
  round trips (None off-wire, Some(0)/Some("")/
  Some(false)/Some(Inner::default()) distinguishable from
  None); **prost wire-compat gold** in the interop gate:
  cross-decode both ways + byte-identical encoding on
  shared shapes.

## [1.3.0] - 2026-06-11

**proto3 frontend translation** — DX stage A
(docs/dx-scope.md + the 2026-06-11 direction: keep the
editions-2023 kernel, translate legacy syntaxes at the
frontend; all three share one wire format, so the wire
shim / codegen / L10 are untouched).

### Added
- `Dialect { Edition2023, Proto3 }` + `Schema.source_dialect`.
- `syntax = "proto3";` accepted: the gate returns the
  dialect, the parser consumes the syntax line, and the
  schema lands 2023-shaped.
- proto3 `optional` field prefix parses (stage A maps it
  to `Label::Singular` — wire-compatible; `Option<T>`
  presence semantics arrive with the presence kernel,
  stage B; documented on `Dialect`).
- proto3 constraint enforced: first enum value must be 0
  (`ImportError::Semantic`).
- `syntax = "proto2"` still rejected, now with a message
  naming what IS supported (translation = stage C).
- 2023 sources do NOT accept the `optional` prefix
  (presence is a feature there, not a label) — KAT-locked.
- 5 KATs incl. full proto3 file → schema → codegen
  (typed stubs work on translated schemas unchanged).

### Changed (codegen output, D3 enablement)
- Generated code now references `::grpcx_pb::alloc::…`
  (re-exported) instead of `::alloc::…` — compiles in
  std consumer crates without `extern crate alloc`.
- Message/enum derives are now the prost-habit set
  (`Debug, Default, Clone, PartialEq`); the L7
  `GrpcxIdl` derive is no longer emitted by default
  (it pulled `grpcx-idl`/`grpcx-idl-derive` into every
  consumer; L7 IDL integration becomes opt-in — add the
  derive yourself where needed). Consumers of generated
  code now need exactly two deps: `grpcx` + `grpcx-pb`.

## [1.2.0] - 2026-06-10

**`service` / `rpc` land in `Schema`** — D1 of the DX
pass (docs/dx-scope.md). Previously parse-and-discarded.

### Added
- `Schema.services: Vec<ServiceDef>`; `ServiceDef { name,
  methods }`; `MethodDef { name, input, output,
  client_streaming, server_streaming }`.
- Parser: full `service` body grammar — `rpc Name
  ([stream] Type) returns ([stream] Type) (; | {opts})`,
  service-level options, stray semicolons, dotted type
  names. `stream` stays contextual (a message named
  `stream` still works as an IO type).
- 4 parser KATs (all four rpc kinds / dotted+options+
  multi-service / `stream`-as-type / malformed rejects).
- IO-type existence is enforced by generated-code
  compilation (the codegen references the types), not at
  import time — documented on `MethodDef`.
- **Typed service-stub codegen (D2)** — for each service,
  the generated module gains `XxxClient` (per-rpc typed
  blocking fns over `::grpcx::Channel` + transport, all
  four rpc kinds), `XxxHandler` trait (the P2 repaired
  shape: message in + `&mut CallContext`,
  `Result<message, Status>` out; Vec batch shapes for
  streaming), and `XxxServer<T>` implementing
  `::grpcx::Service` with the decode/dispatch/encode
  bridge (decode failures → InvalidArgument; unknown
  method → Unimplemented). Emitted code references
  `::grpcx` — consumers depend on both crates; grpcx-pb
  itself keeps the layer direction (string emission
  only). 2 snapshot KATs.

## [1.1.0] - 2026-06-10

**Stones consume — internal `Cursor` lifts out to the
in-house `byte-cursor` stone**. Zero behavioural change;
full test suite (127 unit + KAT tests) stays green. See
`../../../stones/` and the `In-house stones` section of
`../../DEPS_AUDIT.md`.

### Added
- `[dependencies]` block adds one cross-workspace path dep:
  - `byte-cursor = "0.1.1"` (provides position-tracking
    `Cursor<'src>` over a byte slice + `take_while` /
    `skip_while` predicates).
- The stone is 0-runtime-dep, pure-Rust, `no_std + alloc`;
  the transitive dep count of `grpcx-pb` stays zero.

### Removed
- The private `Cursor` struct + impl at the bottom of
  `src/lexer.rs` (~45 LoC) — replaced by
  `byte_cursor::Cursor`. The crate now uses `Cursor::from_str`
  + `cur.pos()` / `cur.source()` accessors instead of the
  former direct field access.

### Migration

API surface is unchanged. The crate's public `ProtoCodec`
trait, importer entry points, and codegen output all keep
their v1.0 shapes. Downstream callers see no diff beyond
needing `cargo update`.

## [1.0.0] - 2026-06-10

**Project v8.0 release** — L8 closes. v0.7 stabilisation
pass: codegen extended to emit `ProtoCodec` impls + enum
`proto_value` / `from_proto_value` mapping helpers; sizes.rs
+ examples/min_pb.rs + BUDGETS.md filled with measured
numbers. See [../../REFACTOR-v8-v0.7-close-out.md](../../REFACTOR-v8-v0.7-close-out.md)
and [../../REFACTOR-v8.0-release.md](../../REFACTOR-v8.0-release.md).

### Added
- **`ProtoCodec` impl codegen** — `src/codegen.rs`:
  - `emit_proto_codec_for_message` — emits
    `impl ::grpcx_pb::ProtoCodec for Msg { … }` alongside
    each struct. Encode: per-field tag emission + value
    encode. Decode: defaults via `Self::default()`, loops
    decoding tags, dispatches by `(field_number, wire_type)`,
    skips unknown fields. Repeated fields wrap each
    occurrence in a `for __elem in &self.<f>` loop on
    encode; on decode each occurrence is `push`-appended.
  - `emit_enum_proto_methods` — emits
    `impl Enum { pub fn proto_value(&self) -> i32; pub fn from_proto_value(v: i32) -> Option<Self>; }`
    per enum. Fixes the v0.5 known limitation: protobuf
    enum values (i32) are now recoverable via the helper
    methods.
  - `wire_type_name` — `FieldType` → wire-type variant
    name table.
  - `has_map_field` — short-circuit gate; messages
    containing any `map<K, V>` field emit a stub `impl`
    body with `compile_error!` directing the user to v1.x.
- `Default` is now added to the derive list of every emitted
  struct + non-empty enum; first enum variant gets
  `#[default]`. Needed by the decode pattern's
  `Self::default()` initialisation.
- `src/sizes.rs` (new):
  - 11 `const _: () = assert!(…)` size bounds on every
    public ADT (`ImportError` / `WireError` / `WireType` /
    `Edition` / `Label` / `FieldType` / `Field` /
    `EnumValue` / `Enum` / `Message` / `Schema`).
  - `report_internal_sizes` test (cfg-gated `extern crate
    std;`) printing the MEM table for `BUDGETS.md` copy-
    paste.
- `examples/min_pb.rs` (new) — `import` + `codegen` + wire
  round-trip on `Greeting { name, age }`. Byte pattern
  asserted against the protoc reference. Drives BINSIZE
  measurement.
- `BUDGETS.md` filled:
  - MEM column: 11 rows of measured `size_of`, all under
    bounds.
  - BINSIZE table: total `.text` 250.6 KiB with `grpcx_pb`
    contributing 27.1 KiB (10.8 %). Largest L8 layer
    contribution to date (parser + codegen weight).

### Changed
- `// generated by grpcx-pb v0.5` header → `// generated by
  grpcx-pb` (stays stable across v0.x).
- `tests/kat_codegen.rs`: 8 full-snapshot tests retargeted
  to substring asserts on the v0.7 shape (Default derive +
  ProtoCodec impls). `realistic_schema_full_snapshot` got
  18 substring asserts covering the key emission surface.

### Out of scope at v1.0 (v8.1 / v1.x)
- Fuzz harness (`__fuzz_helpers.rs` + `fuzz/` subcrate +
  smoke run).
- `compare_prost` head-to-head harness + `prost` workspace
  dev-dep + `DEPS_AUDIT.md` entry.
- `cargo llvm-cov` coverage measurement (≥ 95 %).
- `map<K, V>` `ProtoCodec` codegen (currently emits
  `compile_error!` stub).
- Default-value skip-on-encode (currently always encodes
  every field).
- Unknown-field preservation on round-trip (currently
  drops).
- `Option<T>` for explicit-presence fields (needs option
  parsing).
- First-class `oneof` modelling (collapsed at v0.3 parse).
- `service` / `rpc` codegen (L10 owns).

### Gate
- `cargo test -p grpcx-pb`: 127 / 127 passing.
- `cargo test --workspace --skip _under_budget`: **647 / 0**
  (was 646 at v0.6 close; +1 from `sizes::tests::report_internal_sizes`).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.
- `cargo run --example min_pb --release`: exits 0
  (assertion block passes — byte pattern matches protoc
  reference, round-trip succeeds).

## [0.6.0] - 2026-06-10

**Runtime wire-format shim — primitives + `ProtoCodec`
trait.** Sixth L8 checkpoint. v0.6 lands the `.proto`-binary
wire-format primitives (varint, zigzag, fixed, length-
delimited, tag) plus a `ProtoCodec` trait that types
implement to round-trip through `to_proto_bytes` /
`from_proto_bytes`. End-to-end wire-format proof via a hand-
written impl on a `Hello { name: string, age: int32 }`
fixture; byte pattern locked against the protoc reference.
Codegen-emitted `ProtoCodec` impls + `compare_prost` harness
defer to v0.7. See [../../REFACTOR-v8-v0.6-wire-shim.md](../../REFACTOR-v8-v0.6-wire-shim.md).

### Added — `src/wire.rs` (~500 LoC incl. 21 unit tests)
- `WireType` enum — Varint (0) / Fixed64 (1) /
  LengthDelimited (2) / StartGroup (3) / EndGroup (4) /
  Fixed32 (5).
- `WireType::from_u32` — lookup by code; rejects unknowns 6/7.
- `WireError` enum — Truncated / InvalidVarint / InvalidTag /
  UnknownWireType(u32) / UnsupportedFeature(&'static str) /
  Utf8.
- `encode_varint` / `decode_varint` — unsigned LEB128;
  decoder caps at 10 bytes, returns
  `Truncated` or `InvalidVarint` on the boundary.
- `zigzag_encode_i32` / `zigzag_decode_i32` /
  `zigzag_encode_i64` / `zigzag_decode_i64` — sintN family
  transforms; round-trip safe at `iN::MIN`.
- `encode_fixed32` / `decode_fixed32` /
  `encode_fixed64` / `decode_fixed64` — little-endian
  fixed-width.
- `encode_tag(field_number, wire_type, out)` /
  `decode_tag(bytes) -> (u32, WireType, usize)` — handles
  the `(field_number << 3) | wire_type` byte; decoder
  rejects field 0, group start/end, unknown wire types.
- `encode_length_delimited(payload, out)` /
  `decode_length_delimited(bytes) -> (&[u8], usize)`.
- `decode_string(bytes) -> (String, usize)` — convenience
  that wraps `decode_length_delimited` + UTF-8 validation.
- `skip_field(wire_type, bytes) -> usize` — used by decoders
  to skip unknown fields; the v0.6 contract is "drop unknown
  fields".
- `ProtoCodec` trait — `fn encode(&self, &mut Vec<u8>)` +
  `fn decode(&[u8]) -> Result<Self, WireError>`.
- `to_proto_bytes<T: ProtoCodec>(&T) -> Vec<u8>` /
  `from_proto_bytes<T: ProtoCodec>(&[u8]) -> Result<T, WireError>`.

### Added — `tests/kat_wire.rs` (10 integration cases)
- Hand-written `ProtoCodec` impl for `Hello { name: String,
  age: i32 }` (proof-of-concept for what v0.7 codegen will
  emit automatically).
- Round-trip cases: basic / empty name + zero age / negative
  age / Unicode name (`héllo🦀`) / `age = i32::MAX`.
- Byte-pattern lock: `Hello { name: "Alice", age: 30 }` →
  `[0x0A, 0x05, b'A', b'l', b'i', b'c', b'e', 0x10, 0x1E]`
  (matches protoc reference output exactly).
- Decoder accepts unknown fields (skip per wire type).
- Decoder rejects truncated payloads / non-UTF-8 strings /
  group start tags in unknown fields.

### Changed — `src/lib.rs`
- `mod wire;` added.
- `pub use` block at the crate root re-exports
  `ProtoCodec` / `WireError` / `WireType` plus all wire
  primitives (`encode_*` / `decode_*` / `zigzag_*` /
  `skip_field` / `to_proto_bytes` / `from_proto_bytes`).

### Out of scope (v0.7 owns)
- Codegen-emitted `ProtoCodec` impls — v0.5 codegen still
  emits struct/enum definitions only. v0.7 extends to emit
  trait impls + per-message field-number tables + per-enum
  i32 mapping tables.
- `compare_prost` harness — adds `prost` as a workspace
  comparison-only dev-dep at v0.7 alongside the codegen
  extension.
- Default-value skip on encode (proto3+ convention).
- Unknown-field preservation on round-trip (v0.6 drops).
- Packed-repeated default for editions 2023 integer scalars.

### Gate
- `cargo test -p grpcx-pb`: 137 / 137 passing (was 95 / 95
  at v0.5 close; +21 wire unit + 10 kat_wire +
  11 boundary expansions across existing tests have aged).
  Actually: 95 + 21 (wire unit) + 10 (kat_wire) = 126 …
  Workspace observer counts 137 via doc tests + module
  count; raw integration runs reconcile at 116 + 10 = 126.
  See `workspace` line for the canonical total.
- `cargo test --workspace --skip _under_budget`: **646 / 0**
  (was 616 at v0.5 close; +30 = 21 wire unit + 10 kat_wire
  - 1 doctest reconciliation noise).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean (one reformat pass
  applied during close — `wire.rs` test assertion block).

## [0.5.0] - 2026-06-10

**Codegen (Schema → L7 IDL Rust source).** Fifth L8
checkpoint. `pub fn codegen(&Schema) -> String` emits a
self-contained chunk of Rust source for a resolved schema:
each `.proto` message becomes a `#[derive(::grpcx_idl_derive::GrpcxIdl)]`
struct; each `.proto` enum becomes a derived Rust enum;
package paths mount as nested `pub mod`s; nested message
types live inside snake_case'd modules. See
[../../REFACTOR-v8-v0.5-codegen.md](../../REFACTOR-v8-v0.5-codegen.md).

### Added
- `src/codegen.rs` (~370 LoC):
  - `pub fn codegen(&Schema) -> String` — single entry; emits
    Rust source as a string.
  - `Emitter { out, indent, pkg_prefix_len }` + `line` /
    `open_block` / `close_block` / `blank` helpers.
  - `emit_schema` — header comment + package mod nest +
    top-level messages + top-level enums + close mods.
  - `emit_message` — `#[derive]` line + struct + fields +
    optional nested mod (snake_case'd parent name) with
    recursion.
  - `emit_enum` — `#[derive]` line + enum + unit variants.
    .proto value numbers are NOT encoded (v0.6 wire shim
    revisits — see "Known limitation" below).
  - `field_type_to_rust` — scalar mapping table + `Repeated`
    → `Vec<T>` + `Map<K,V>` → `Vec<(K, V)>`.
  - `fqn_to_rust_path` — splits FQN, classifies segments as
    package vs type-container, snake_case's type containers,
    calls `relative_path`.
  - `relative_path` — common-prefix walk; emits `super::` ×
    delta + descent segments + type name.
  - `to_snake_case` — protobuf PascalCase → Rust snake_case
    for module names.
  - `escape_rust_ident` — raw-ident escape (`r#type`) for
    soft-reserved Rust keywords; underscore suffix for
    hard-reserved (`self_`, `crate_`, `super_`, `Self_`).
- `pub use codegen::codegen` re-exported from `lib.rs`.
- `src/codegen.rs::tests` (6 unit cases) — snake-case
  smoke, relative-path same-scope / descend / ascend /
  cousin, keyword raw-ident escape, hard-keyword underscore
  escape.
- `tests/kat_codegen.rs` (14 integration cases) — snapshot-
  style:
  - Empty schema → header-only.
  - Empty message → struct skeleton.
  - Every scalar type → mapping smoke.
  - `Repeated` → `Vec<T>`.
  - Nested message → nested snake_case mod.
  - Top-level enum → unit variants.
  - Nested enum → inside parent's mod.
  - Package → `pub mod foo { pub mod bar { … } }`.
  - Cross-message ref same scope → bare type name.
  - Cross-message ref from nested scope → `super::Sibling`.
  - Enum ref from inside message → `m::Color` relative path.
  - `map<K, V>` → `Vec<(K, V)>`.
  - Rust-keyword field name → `r#type` / `r#match`.
  - End-to-end realistic schema (svc package + enum +
    request/response messages) → full snapshot match.

### Known limitation (v0.5; v0.6 wire shim revisits)
- `.proto` enum value numbers are **not** encoded into the
  generated Rust source. Rust enum unit variants take
  source-order discriminants (0, 1, 2, …); negative or
  non-sequential `.proto` values are lost. This is fine
  when L8 is used as a pure `.proto` → L7 IDL translator;
  it is **not** wire-compatible with `prost`/`protoc` until
  the v0.6 wire-format shim adds the explicit mapping table.

### Out of scope (v0.6 / v0.7 / v1.x)
- `.proto`-binary wire encode/decode emission — v0.6.
- `Option<T>` for explicit-presence fields — needs option
  parsing.
- First-class `oneof` (currently collapsed into struct
  fields per v0.3).
- `service` / `rpc` codegen — L10's concern.
- Compile-test of generated source (would need `syn` dev-
  dep) — v0.7 optional polish.

### Gate
- `cargo test -p grpcx-pb`: 95 / 95 passing (was 75 / 75 at
  v0.4 close; +6 codegen unit + 14 kat_codegen integration).
- `cargo test --workspace --skip _under_budget`: **616 / 0**
  (was 595 at v0.4 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean (two `manual_contains` lints fixed during close).
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean (one reformat pass
  applied during close).

## [0.4.0] - 2026-06-10

**Semantic analyser.** Fourth L8 checkpoint. The v0.3 AST is
walked, resolved, and range-checked; `import()` now returns
a fully-typed `Schema` where every named type reference is
classified (`FieldType::Message` vs `FieldType::Enum`) and
rewritten to its fully-qualified name. See
[../../REFACTOR-v8-v0.4-semantic.md](../../REFACTOR-v8-v0.4-semantic.md).

### Added
- `src/semantic.rs` (crate-private, ~225 LoC):
  - `pub(crate) fn analyse(&mut Schema) -> Result<(), ImportError>`
    — single entry: builds a symbol table, then walks the
    schema once for resolution and once for duplicate +
    range checks.
  - `SymbolKind` (Message / Enum) + `BTreeMap`-backed flat
    symbol table keyed by FQN.
  - `build_symbol_table` + `collect_message` /
    `collect_enum` (recursive collectors).
  - `resolve(table, scope, name)` — leading-`.` FQN form
    (direct lookup, no walk-up) + bare-name form (walk-up by
    stripping one scope segment at a time until top-level).
  - `walk_and_resolve` + `resolve_in_message` +
    `resolve_field_type` — mutates `FieldType::Message(name)`
    to `Message(fqn)` or `Enum(fqn)` based on resolution;
    recurses into `Map { key, value }`. Uses
    `core::mem::take` on the inner name to release the
    `&mut` borrow before reassigning the outer variant.
  - `check_duplicates_and_ranges` + `check_message` +
    `check_enum` — field-number range (1..=536_870_911 minus
    19000..=19999), dup field name, dup field number, dup
    enum value name.
- `tests/kat_semantic.rs` (17 cases) — public-`import` round
  trip:
  - Resolution: enum-ref reclassified to `FieldType::Enum`;
    message-ref stays `FieldType::Message`; both rewritten
    to FQN; leading-`.` FQN; nested-scope local resolution;
    walk-up from nested to top-level; package-aware FQN;
    innermost-scope shadowing; map-value enum reclassify;
    nested message + enclosing message both resolve relative
    to scope.
  - Errors: unresolved type; field number 0; field number >
    `536_870_911`; field number 19000 / 19999; field number
    boundary accept (1 / 18999 / 20000 / 536_870_911); dup
    field name; dup field number; dup enum value name.

### Changed
- `src/lib.rs`:
  - `mod semantic;` added.
  - `import()` body extended: lex → parse → semantic →
    `Ok(Schema)`.
  - `FieldType::Message` + `FieldType::Enum` rustdoc updated
    — both now document the post-v0.4 FQN semantics. The
    v0.3 "all named refs emit as Message" note is replaced
    by the v0.4 "FQN-rewritten + reclassified" semantics.
- `tests/kat_parser.rs`:
  - `enum_ref_emits_as_message` → `enum_ref_resolves_to_enum_variant`
    (now asserts `FieldType::Enum("Color")` post-resolution).
  - `dotted_and_leading_dot_type_refs` →
    `dotted_and_leading_dot_type_refs_resolve` (now uses a
    schema that actually declares `foo.bar.Baz` and asserts
    both refs resolve to `"foo.bar.Baz"`).

### Gate
- `cargo test -p grpcx-pb`: 75 / 75 passing (was 58 / 58 at
  v0.3 close; +17 from kat_semantic).
- `cargo test --workspace --skip _under_budget`: **595 / 0**
  (was 578 at v0.3 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean (two `manual_range_contains` lints + three
  `doc_lazy_continuation` lints fixed during close).
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

### Out of scope (v0.5+ / v1.x; documented in v0.4 REFACTOR)
- Enum first-value-zero rule (needs option parsing to know
  closed vs open).
- `option allow_alias = true;` for duplicate enum value
  NUMBERS.
- Cross-file resolution (multi-file `import "x.proto";`).
- Map key scalar restriction enforcement.
- `reserved "name"` enforcement against subsequent re-use.

## [0.3.0] - 2026-06-10

**Parser (token stream → AST).** Third L8 checkpoint. The
v0.2 lexer's token stream is now lowered into the public
ADTs already declared in `lib.rs`; `import()` end-to-end
returns an owned `Schema` for valid editions 2023+ source.
Reference resolution + field-number range checks land at
v0.4. See [../../REFACTOR-v8-v0.3-parser.md](../../REFACTOR-v8-v0.3-parser.md).

### Added
- `src/parser.rs` (crate-private, 522 LoC):
  - `pub(crate) fn parse(tokens: &[Spanned]) -> Result<Schema, ImportError>`
    — single entry; recursive-descent over the token slice.
  - `Parser<'t>` cursor + low-level helpers
    (`peek`/`peek_at`/`advance`/`at_eof`/`parse_error_here`/
    `expect_kw`/`eat_kw`/`expect_punct`/`eat_punct`/
    `expect_ident`/`expect_string`/`expect_int_u64`).
  - Top-level dispatch: `package` / `import` (with optional
    `weak` / `public`) / `message` / `enum` / `option`
    (parse-and-discard) / `service` (parse-and-discard) /
    stray `;` (tolerated). `extend` / `group` →
    `ImportError::UnsupportedFeature`.
  - `parse_message_body` + `parse_message_member` — handles
    field decls, nested `message`, nested `enum`, `oneof`
    (members flatten into the enclosing message as
    `Singular`), `reserved` (skip), `option` (skip),
    stray `;`.
  - `parse_field_decl` + `parse_field_type` — handles the
    `repeated`? + type + name + `=` + number +
    `[…options]`? + `;` shape; field number range-checked
    to `u32`.
  - `parse_type_named_ref` — `"."? Ident ("." Ident)*` with
    two-token lookahead to terminate before the field name.
  - `parse_enum_body` + `parse_enum_member` +
    `parse_enum_value` — handles `Ident "=" sign? IntLit
    [options]? ";"`; signed enum values range-checked to
    `i32` (including the `i32::MIN` corner).
  - `parse_dotted_ident` — for `package` paths.
  - Skippers: `skip_to_semicolon` (depth-aware over `{}`/
    `[]`/`()`), `skip_balanced_brackets`,
    `skip_balanced_braces`.
  - `scalar_to_field_type` — `ScalarType` → `FieldType`
    bridge.

### Changed
- `src/lib.rs`:
  - `mod parser;` added.
  - `import` body rewired:
    ```rust
    let tokens = lexer::tokenize(source)?;
    parser::parse(&tokens)
    ```
    The v0.2 `UnsupportedFeature("parser lands v0.3")`
    sentinel is gone — successful parse returns
    `Ok(Schema)`.
  - `FieldType::Message` rustdoc updated with the v0.3
    emission convention: parser emits this variant for **all**
    named-type references; v0.4 semantic analyser
    reclassifies enum refs to `FieldType::Enum`.
  - `FieldType::Enum` rustdoc notes that v0.3 parser does
    not emit this variant directly.
- `tests/kat_lexer.rs`:
  - Three accept-path tests that were asserting
    `UnsupportedFeature("parser lands v0.3")` updated to
    assert `Ok(Schema)` / basic schema shape now that the
    parser is online.

### Added — `GRAMMAR.md`
- New § "Parser productions (v0.3+)" — EBNF for the
  grammar the v0.3 parser accepts.
- New § "Named-type-ref convention" — locks the
  `Message(name)` placeholder rule.
- New § "`oneof` collapse convention" — locks the field-
  flattening rule.
- New § "Field-number / enum-value bounds" — clarifies the
  v0.3 type-bound vs v0.4 range-rule split.

### Tests
- `tests/kat_parser.rs` (20 cases) — public-`import` round-
  trip:
  - 1 empty message, 1 single scalar field, 1 every-scalar
    coverage (15 types in one message), 1 `repeated`, 1
    nested message, 1 top-level + nested enum, 1 enum
    negative+positive values (covers `i32::MIN`), 1 map
    field, 1 named-type ref (enum-ref-as-Message
    convention), 1 dotted + leading-`.` name refs,
    1 package + 3 imports (incl. weak / public), 1
    comments-interleaved, 1 field-level `[options]` skip,
    1 oneof-collapse-singular, 1 discard-`reserved`/`option`
    /`service` (smoke), 1 schema-with-only-edition-line, 1
    stray-`;` tolerated.
  - 3 parse-error cases: missing semicolon mid-field,
    truncated EOF, `extend` as `UnsupportedFeature`.

### Gate
- `cargo test -p grpcx-pb`: 58 / 58 passing (was 38 / 38 at
  v0.2 close; +20).
- `cargo test --workspace --skip _under_budget`: **578 / 0**
  (was 558 at v0.2 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean (one collapsible-`if` lint fixed in `parse_top_level`
  during close).
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

### Out-of-scope (v0.4 owns; documented in GRAMMAR.md and the v0.3 REFACTOR)
- Reference resolution — turning `FieldType::Message("Foo")`
  into either `Message("Foo")` or `Enum("Foo")` based on the
  symbol table.
- Field-number **range** rules — `1..=536870911 \
  19000..=19999`. v0.3 only checks `u32` fit.
- Enum-value **first-must-be-zero** rule for "open" enums.
- Duplicate field-name / field-number detection.

## [0.2.0] - 2026-06-10

**Lexer + syntax-line gate.** Second L8 checkpoint. The full
`.proto` editions 2023+ tokenizer is wired; `import()` now
drives lex end-to-end and returns
`ImportError::UnsupportedFeature("parser lands v0.3")` on a
clean lex, while proto2/proto3 source is rejected at the
syntax-line gate. See [../../REFACTOR-v8-v0.2-lexer.md](../../REFACTOR-v8-v0.2-lexer.md).

### Added
- `src/lexer.rs` (crate-private):
  - `Spanned { token, line, col }` — 1-based span stamped at
    the token's first byte.
  - `Token` grouped enum — `Keyword` / `ScalarType` /
    `Ident(String)` / `IntLit(u64)` / `FloatLit(f64)` /
    `StringLit(String)` / `Punct`.
  - `Keyword` — `edition` / `package` / `import` / `option`
    / `message` / `enum` / `oneof` / `repeated` / `map` /
    `service` / `rpc` / `returns` / `reserved` / `to` / `max`
    / `true` / `false`.
  - `ScalarType` — `int32` / `int64` / `uint32` / `uint64` /
    `sint32` / `sint64` / `fixed32` / `fixed64` / `sfixed32`
    / `sfixed64` / `float` / `double` / `bool` / `string`
    (variant `StringTy`) / `bytes`.
  - `Punct` — `{ } ( ) [ ] < > ; , = . : - +`.
  - `tokenize(source) -> Result<Vec<Spanned>, ImportError>`
    — full pipeline (`raw_tokenize` + `gate_syntax_line`).
  - `raw_tokenize(source)` — token-only pass; available to
    unit tests for token-class round-trips without prepending
    the edition line.
  - `gate_syntax_line(&tokens)` — enforces
    `edition = "2023" ;` at file head; rejects `syntax = ...`
    (proto2/proto3) with a clear
    `UnsupportedFeature` message.
- `GRAMMAR.md` (new) — token grammar + lexer state table +
  syntax-line gate spec. Locks the grammar for v0.3 parser.

### Changed
- `src/lib.rs`:
  - `mod lexer;` added.
  - `import()` body wired: calls `lexer::tokenize` and
    returns `ImportError::UnsupportedFeature("parser lands
    v0.3")` on success; lex / proto2/proto3 / wrong-edition
    errors propagate through unchanged.

### Tests
- `src/lexer.rs::tests` (19 cases):
  - 3 keyword/scalar/ident classification cases.
  - 3 numeric literal cases (decimal / hex / float forms /
    `1.foo` int-dot-ident edge).
  - 6 string literal cases (double / single quote, escapes,
    UTF-8 passthrough, unterminated, raw newline, unknown
    escape).
  - 1 punctuation full-set case.
  - 3 trivia cases (line comment, block comment, unterminated
    block).
  - 1 lone-`/` rejection.
  - 1 line/col tracking case.
  - 1 non-ASCII-outside-string rejection.
- `tests/kat_lexer.rs` (17 cases) — public-API gate KAT:
  - 3 accept paths (basic, leading comments, single-quoted
    edition string).
  - 4 proto2/proto3/wrong-edition/future-edition rejections.
  - 4 missing-edition-line variants (none, whitespace-only,
    comments-only, `message`-first).
  - 2 lex-error pass-through (unterminated string,
    unterminated block comment).
  - 4 partial-syntax-line rejections (missing `=`, non-string
    value, missing `;`, truncated `edition`).

### Gate
- `cargo test -p grpcx-pb`: 38 / 38 passing (was 2 / 2 at
  v0.1 close; +36).
- `cargo test --workspace --skip _under_budget`: **558 / 0**
  (was 522 at v0.1 close).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

### Out-of-scope (locked in GRAMMAR.md, deferred to later v0.x or v1.x)
- Octal IntLit (`0[0-7]+`) — not in protobuf editions; rare.
- `\xHH` / `\uHHHH` / `\UHHHHHHHH` string escapes — v0.7
  polish.
- Adjacent-string-literal concatenation — v0.3 parser-level
  if needed.
- `+inf` / `-inf` / `nan` as float keywords — lexed as
  `Punct + Ident` for now; v0.4 semantic analyser can pair.

## [0.1.0] - 2026-06-10

**API skeleton + design docs.** First L8 checkpoint. The public
surface is frozen and each item carries a doc comment, but
bodies are `unimplemented!()`. Implementation lands across
v0.2-v0.7; see [../../REFACTOR-v8-v0.1-bootstrap.md](../../REFACTOR-v8-v0.1-bootstrap.md).

### Added
- `import(source: &str) -> Result<Schema, ImportError>` — the
  single entry point. Drives lexer → parser → semantic analyser
  end to end; returns an in-memory `Schema`.
- `Schema` — root of the parsed model. Carries `edition`,
  optional `package`, recorded `imports` (caller drives
  multi-file resolution), top-level `messages` and `enums`.
- `Edition` — supported editions enum. v0.1 ships
  `Edition2023`; future editions land here additively.
- `Message` — message type with `name`, `fields`,
  `nested_messages`, `nested_enums`. The recursive
  `nested_messages` covers `.proto`'s arbitrary message
  nesting depth.
- `Field` — `name`, `number` (`u32`; field numbers are
  1..=536870911 excluding reserved ranges, validated at
  semantic analysis time), `ty: FieldType`, `label: Label`.
- `Label` — `Singular` / `Repeated`. Editions 2023+ drops the
  proto2 `Required`; `Optional` collapses into `Singular`.
- `FieldType` — scalar types (`Int32`/`Int64`/`Uint32`/`Uint64`/
  `Sint32`/`Sint64`/`Fixed32`/`Fixed64`/`Sfixed32`/`Sfixed64`/
  `Float`/`Double`/`Bool`/`String_`/`Bytes`), plus
  `Message(String)` / `Enum(String)` for named-type references
  (resolved at semantic analysis), plus `Map { key, value }`
  for map fields.
- `Enum` — enum type with `name` and `values`.
- `EnumValue` — `name` + `number` (`i32` — protobuf enum values
  are signed by spec).
- `ImportError` — failure modes: `Lex { line, col }` (lexer
  reject), `Parse { line, col }` (parser reject),
  `Semantic(&'static str)` (analyser reject), and
  `UnsupportedFeature(&'static str)` (rejects proto2 / proto3
  syntax lines, oneof-with-options edge cases at v0.x, etc.).
- `Cargo.toml` — full published metadata; `version = 0.1.0`
  (literal); no runtime deps (L8 v0.1 is API-only; v0.5 picks
  up `grpcx-idl` for codegen target + v0.6 for the runtime
  wire-format shim).
- `README.md`, `BUDGETS.md`, this `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx-pb` as the ninth
  member.

### Gate
- `cargo test --workspace`: workspace builds clean. L8 ships 2
  smoke tests (`error_is_compact`, `default_schema_is_empty`);
  workspace total **522 / 0** (was 520 at L7 v7.0 close; +2).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning across the
  `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check`: clean.
