//! v0.3 KAT — parser cases via the public `import` entry.
//!
//! Each case feeds `.proto` editions 2023+ source through
//! `import(&str) -> Result<Schema, ImportError>` and asserts
//! on the resulting `Schema` shape (or `ImportError` variant
//! for the reject cases).
//!
//! Parser-level conventions baked into these expectations:
//! - **Named-type refs emit `FieldType::Message(name)`** at
//!   the v0.3 parse stage; v0.4 semantic analyser
//!   reclassifies enum refs. The "enum_ref_emits_as_message"
//!   case is the contract.
//! - **`oneof` members collapse into `Message.fields`** as
//!   `Label::Singular`. The "oneof_fields_collapse_singular"
//!   case is the contract.
//! - **`option` / `reserved` / `service` blocks** are parse-
//!   and-discarded; surrounding declarations still land in
//!   the AST.

use grpcx_pb::{Edition, FieldType, ImportError, Label, import};

// ============================================================
// Helpers
// ============================================================

fn ok(src: &str) -> grpcx_pb::Schema {
    import(src).unwrap_or_else(|e| panic!("expected Ok(Schema) for {src:?}, got {e:?}"))
}

// ============================================================
// 1. Empty message.
// ============================================================

#[test]
fn empty_message() {
    let s = ok("edition = \"2023\";\nmessage Empty {}\n");
    assert_eq!(s.edition, Edition::Edition2023);
    assert_eq!(s.messages.len(), 1);
    let m = &s.messages[0];
    assert_eq!(m.name, "Empty");
    assert!(m.fields.is_empty());
    assert!(m.nested_messages.is_empty());
    assert!(m.nested_enums.is_empty());
}

// ============================================================
// 2. Single scalar field.
// ============================================================

#[test]
fn single_scalar_field() {
    let s = ok("edition = \"2023\";\nmessage Greeting { string name = 1; }\n");
    let m = &s.messages[0];
    assert_eq!(m.fields.len(), 1);
    let f = &m.fields[0];
    assert_eq!(f.name, "name");
    assert_eq!(f.number, 1);
    assert_eq!(f.ty, FieldType::String_);
    assert_eq!(f.label, Label::Singular);
}

// ============================================================
// 3. Mixed scalar fields — every scalar at least once.
// ============================================================

#[test]
fn mixed_scalar_fields_cover_all_types() {
    let src = r#"
edition = "2023";
message Mixed {
  int32    a  = 1;
  int64    b  = 2;
  uint32   c  = 3;
  uint64   d  = 4;
  sint32   e  = 5;
  sint64   f  = 6;
  fixed32  g  = 7;
  fixed64  h  = 8;
  sfixed32 i  = 9;
  sfixed64 j  = 10;
  float    k  = 11;
  double   l  = 12;
  bool     m  = 13;
  string   n  = 14;
  bytes    o  = 15;
}
"#;
    let s = ok(src);
    let m = &s.messages[0];
    let types: alloc::vec::Vec<&FieldType> = m.fields.iter().map(|f| &f.ty).collect();
    assert_eq!(
        types,
        vec![
            &FieldType::Int32,
            &FieldType::Int64,
            &FieldType::Uint32,
            &FieldType::Uint64,
            &FieldType::Sint32,
            &FieldType::Sint64,
            &FieldType::Fixed32,
            &FieldType::Fixed64,
            &FieldType::Sfixed32,
            &FieldType::Sfixed64,
            &FieldType::Float,
            &FieldType::Double,
            &FieldType::Bool,
            &FieldType::String_,
            &FieldType::Bytes,
        ]
    );
    // Field numbers should be the 1..=15 we wrote.
    for (i, f) in m.fields.iter().enumerate() {
        assert_eq!(f.number, (i as u32) + 1);
    }
}

// ============================================================
// 4. Repeated label.
// ============================================================

#[test]
fn repeated_modifier() {
    let s = ok(r#"
edition = "2023";
message R {
  repeated string aliases = 1;
  string solo = 2;
}
"#);
    let m = &s.messages[0];
    assert_eq!(m.fields[0].label, Label::Repeated);
    assert_eq!(m.fields[1].label, Label::Singular);
}

// ============================================================
// 5. Nested message.
// ============================================================

#[test]
fn nested_message() {
    let s = ok(r#"
edition = "2023";
message Outer {
  message Inner {
    string x = 1;
  }
  int32 outer_id = 1;
}
"#);
    let m = &s.messages[0];
    assert_eq!(m.name, "Outer");
    assert_eq!(m.fields.len(), 1);
    assert_eq!(m.nested_messages.len(), 1);
    let inner = &m.nested_messages[0];
    assert_eq!(inner.name, "Inner");
    assert_eq!(inner.fields[0].name, "x");
}

// ============================================================
// 6. Top-level enum + nested enum.
// ============================================================

#[test]
fn enum_top_level_and_nested() {
    let s = ok(r#"
edition = "2023";
enum TopColor {
  RED = 0;
  GREEN = 1;
}
message Wrap {
  enum InnerStatus {
    IDLE = 0;
    BUSY = 1;
  }
}
"#);
    assert_eq!(s.enums.len(), 1);
    let top = &s.enums[0];
    assert_eq!(top.name, "TopColor");
    assert_eq!(top.values.len(), 2);
    assert_eq!(top.values[0].name, "RED");
    assert_eq!(top.values[0].number, 0);
    assert_eq!(top.values[1].name, "GREEN");
    assert_eq!(top.values[1].number, 1);

    let m = &s.messages[0];
    assert_eq!(m.nested_enums.len(), 1);
    assert_eq!(m.nested_enums[0].name, "InnerStatus");
}

// ============================================================
// 7. Enum with negative + positive values.
// ============================================================

#[test]
fn enum_negative_and_positive_values() {
    let s = ok(r#"
edition = "2023";
enum Sign {
  ZERO = 0;
  NEG  = -42;
  POS  = +7;
  MAX  = 2147483647;
  MIN  = -2147483648;
}
"#);
    let e = &s.enums[0];
    let vals: alloc::vec::Vec<(&str, i32)> = e
        .values
        .iter()
        .map(|v| (v.name.as_str(), v.number))
        .collect();
    assert_eq!(
        vals,
        vec![
            ("ZERO", 0),
            ("NEG", -42),
            ("POS", 7),
            ("MAX", i32::MAX),
            ("MIN", i32::MIN),
        ]
    );
}

// ============================================================
// 8. Map field.
// ============================================================

#[test]
fn map_field() {
    let s = ok(r#"
edition = "2023";
message M {
  map<string, int32> counts = 1;
}
"#);
    let m = &s.messages[0];
    match &m.fields[0].ty {
        FieldType::Map { key, value } => {
            assert_eq!(**key, FieldType::String_);
            assert_eq!(**value, FieldType::Int32);
        }
        other => panic!("expected Map<string,int32>, got {other:?}"),
    }
}

// ============================================================
// 9. Named-type ref to an enum — v0.4 reclassifies to Enum.
// ============================================================

#[test]
fn enum_ref_resolves_to_enum_variant() {
    // Up to v0.3 the parser emitted FieldType::Message for
    // every named ref; v0.4's semantic analyser swaps refs
    // that resolve to enum decls over to FieldType::Enum.
    // Since the public `import` runs lex → parse → semantic,
    // tests now observe the post-resolution shape.
    let s = ok(r#"
edition = "2023";
enum Color { RED = 0; GREEN = 1; }
message Painted {
  Color shade = 1;
  Color outline = 2;
}
"#);
    let m = &s.messages[0];
    for f in &m.fields {
        match &f.ty {
            FieldType::Enum(name) => assert_eq!(name, "Color"),
            other => panic!("expected FieldType::Enum(\"Color\"), got {other:?}"),
        }
    }
}

// ============================================================
// 10. Dotted name-type ref + leading-`.` fully-qualified
//      (post-v0.4: both resolve to the same FQN).
// ============================================================

#[test]
fn dotted_and_leading_dot_type_refs_resolve() {
    let s = ok(r#"
edition = "2023";
package foo.bar;
message Baz {}
message Wrap {
  Baz a = 1;
  .foo.bar.Baz b = 2;
}
"#);
    // Wrap is the second top-level message (Baz is first).
    let wrap = &s.messages[1];
    assert_eq!(wrap.name, "Wrap");
    // Both refs resolve to "foo.bar.Baz" — `Baz` walks up
    // from `foo.bar.Wrap`'s scope to `foo.bar.Baz`; `.foo.bar.Baz`
    // is the leading-dot form of the same FQN.
    assert!(matches!(
        &wrap.fields[0].ty,
        FieldType::Message(n) if n == "foo.bar.Baz"
    ));
    assert!(matches!(
        &wrap.fields[1].ty,
        FieldType::Message(n) if n == "foo.bar.Baz"
    ));
}

// ============================================================
// 11. package + import (+weak/public).
// ============================================================

#[test]
fn package_and_imports() {
    let s = ok(r#"
edition = "2023";
package foo.bar.baz;
import "google/protobuf/any.proto";
import weak "legacy.proto";
import public "shared.proto";
"#);
    assert_eq!(s.package.as_deref(), Some("foo.bar.baz"));
    assert_eq!(
        s.imports,
        vec![
            "google/protobuf/any.proto".to_string(),
            "legacy.proto".to_string(),
            "shared.proto".to_string(),
        ]
    );
}

// ============================================================
// 12. Comments interleaved across body forms.
// ============================================================

#[test]
fn comments_interleaved() {
    let s = ok(r#"
// licence header
edition = "2023";
// top-level comment

message M {
  // field comment
  string name = 1;  // trailing
  /* block
     comment */
  int32  age  = 2;
}
"#);
    let m = &s.messages[0];
    assert_eq!(m.fields.len(), 2);
    assert_eq!(m.fields[0].name, "name");
    assert_eq!(m.fields[1].name, "age");
}

// ============================================================
// 13. Field-level [options] parse-and-discarded.
// ============================================================

#[test]
fn field_options_skipped() {
    let s = ok(r#"
edition = "2023";
message M {
  string name = 1 [deprecated = true, json_name = "n"];
  int32  age  = 2 [(custom.opt) = { nested: "yes" }];
}
"#);
    let m = &s.messages[0];
    assert_eq!(m.fields.len(), 2);
    assert_eq!(m.fields[0].name, "name");
    assert_eq!(m.fields[0].number, 1);
    assert_eq!(m.fields[1].name, "age");
    assert_eq!(m.fields[1].number, 2);
}

// ============================================================
// 14. `oneof` — members collapse into Message.fields.
// ============================================================

#[test]
fn oneof_fields_collapse_singular() {
    let s = ok(r#"
edition = "2023";
message M {
  oneof Choice {
    string text = 1;
    int32  code = 2;
  }
  int32 unrelated = 3;
}
"#);
    let m = &s.messages[0];
    // 3 fields total: the two oneof members + `unrelated`.
    assert_eq!(m.fields.len(), 3);
    assert_eq!(m.fields[0].name, "text");
    assert_eq!(m.fields[0].label, Label::Singular);
    assert_eq!(m.fields[1].name, "code");
    assert_eq!(m.fields[1].label, Label::Singular);
    assert_eq!(m.fields[2].name, "unrelated");
}

// ============================================================
// 15. reserved / top-level option / service — all skipped;
//      surrounding declarations remain.
// ============================================================

#[test]
fn discarded_constructs_dont_consume_surrounding_decls() {
    let s = ok(r#"
edition = "2023";
option features.field_presence = EXPLICIT;
service Greeter {
  rpc SayHi(HelloReq) returns (HelloResp);
}
message Real {
  reserved 5, 7 to 9, "old_name";
  option deprecated = true;
  string ok = 1;
}
"#);
    // Service is parse-and-discarded; only `Real` remains in
    // schema.messages.
    assert_eq!(s.messages.len(), 1);
    let m = &s.messages[0];
    assert_eq!(m.name, "Real");
    // reserved + option blocks discarded; only the real
    // string field survives.
    assert_eq!(m.fields.len(), 1);
    assert_eq!(m.fields[0].name, "ok");
}

// ============================================================
// 16. Parse error — missing semicolon.
// ============================================================

#[test]
fn missing_semicolon_parse_error() {
    let src = "edition = \"2023\";\nmessage M { string a = 1 }\n";
    let err = import(src).unwrap_err();
    assert!(matches!(err, ImportError::Parse { .. }), "got {err:?}");
}

// ============================================================
// 17. Parse error — truncated input at EOF.
// ============================================================

#[test]
fn truncated_message_parse_error() {
    let src = "edition = \"2023\";\nmessage Foo";
    let err = import(src).unwrap_err();
    assert!(matches!(err, ImportError::Parse { .. }));
}

// ============================================================
// 18. extend / group → UnsupportedFeature.
// ============================================================

#[test]
fn extend_unsupported() {
    let src = "edition = \"2023\";\nextend Foo { string x = 1; }\n";
    let err = import(src).unwrap_err();
    assert!(
        matches!(err, ImportError::UnsupportedFeature(_)),
        "got {err:?}"
    );
}

// ============================================================
// Empty input after edition line is fine (trailing whitespace).
// ============================================================

#[test]
fn schema_with_only_edition_line_is_empty() {
    let s = ok("edition = \"2023\";\n");
    assert!(s.messages.is_empty());
    assert!(s.enums.is_empty());
    assert!(s.imports.is_empty());
    assert!(s.package.is_none());
}

// ============================================================
// Stray top-level `;` is tolerated.
// ============================================================

#[test]
fn stray_semicolons_tolerated() {
    let s = ok("edition = \"2023\";\n;\n;;;\nmessage M {}\n;\n");
    assert_eq!(s.messages.len(), 1);
}

extern crate alloc;

// ============================================================
// Services (v1.1 — previously parse-and-discarded).
// ============================================================

#[test]
fn service_with_all_four_rpc_kinds_parses() {
    let schema = grpcx_pb::import(
        r#"edition = "2023";
package greeter.v1;
message Req { string q = 1; }
message Resp { string a = 1; }
service Greeter {
  rpc Unary (Req) returns (Resp);
  rpc SStream (Req) returns (stream Resp);
  rpc CStream (stream Req) returns (Resp);
  rpc Bidi (stream Req) returns (stream Resp);
}
"#,
    )
    .unwrap();
    assert_eq!(schema.services.len(), 1);
    let svc = &schema.services[0];
    assert_eq!(svc.name, "Greeter");
    assert_eq!(svc.methods.len(), 4);
    let flags: Vec<(bool, bool)> = svc
        .methods
        .iter()
        .map(|m| (m.client_streaming, m.server_streaming))
        .collect();
    assert_eq!(
        flags,
        vec![(false, false), (false, true), (true, false), (true, true)]
    );
    assert_eq!(svc.methods[0].input, "Req");
    assert_eq!(svc.methods[0].output, "Resp");
}

#[test]
fn service_dotted_types_options_and_multiple_services() {
    let schema = grpcx_pb::import(
        r#"edition = "2023";
service A {
  option deprecated = true;
  rpc M (foo.bar.In) returns (foo.bar.Out) { option idempotency_level = NO_SIDE_EFFECTS; }
  ;
}
service B {}
"#,
    )
    .unwrap();
    assert_eq!(schema.services.len(), 2);
    assert_eq!(schema.services[0].methods[0].input, "foo.bar.In");
    assert_eq!(schema.services[0].methods[0].output, "foo.bar.Out");
    assert!(schema.services[1].methods.is_empty());
}

#[test]
fn service_message_named_stream_is_a_type_not_a_modifier() {
    // `stream` is contextual: alone in the parens it IS
    // the type name.
    let schema = grpcx_pb::import(
        r#"edition = "2023";
message stream { int32 x = 1; }
service S { rpc M (stream) returns (stream stream); }
"#,
    )
    .unwrap();
    let m = &schema.services[0].methods[0];
    assert!(!m.client_streaming);
    assert_eq!(m.input, "stream");
    assert!(m.server_streaming);
    assert_eq!(m.output, "stream");
}

#[test]
fn service_malformed_rpc_rejected() {
    // Missing `returns`.
    assert!(
        grpcx_pb::import(
            r#"edition = "2023";
service S { rpc M (A) (B); }
"#
        )
        .is_err()
    );
    // Unclosed service body.
    assert!(
        grpcx_pb::import(
            r#"edition = "2023";
service S { rpc M (A) returns (B);
"#
        )
        .is_err()
    );
}

// ============================================================
// proto3 frontend translation (1.3 — DX stage A).
// ============================================================

#[test]
fn proto3_full_file_translates_onto_2023_kernel() {
    let schema = grpcx_pb::import(
        r#"syntax = "proto3";
package shop.v1;
message Item {
  string sku = 1;
  optional string note = 2;
  repeated int32 tags = 3;
}
enum Kind {
  KIND_UNSPECIFIED = 0;
  KIND_BOOK = 1;
}
service Shop { rpc Get (Item) returns (Item); }
"#,
    )
    .unwrap();
    assert_eq!(schema.source_dialect, grpcx_pb::Dialect::Proto3);
    assert_eq!(schema.package.as_deref(), Some("shop.v1"));
    // `optional` prefix maps to Label::Optional (stage B
    // presence kernel): Option<T> + explicit presence.
    let item = &schema.messages[0];
    assert_eq!(item.fields.len(), 3);
    assert_eq!(item.fields[1].name, "note");
    assert_eq!(item.fields[0].label, grpcx_pb::Label::Singular);
    assert_eq!(item.fields[1].label, grpcx_pb::Label::Optional);
    assert_eq!(item.fields[2].label, grpcx_pb::Label::Repeated);
    assert_eq!(schema.services[0].methods[0].name, "Get");
    // Codegen runs unchanged on the translated schema.
    let out = grpcx_pb::codegen(&schema);
    assert!(out.contains("pub struct ShopClient;"));
}

#[test]
fn proto3_enum_first_value_must_be_zero() {
    let err = grpcx_pb::import(
        r#"syntax = "proto3";
enum Bad { FIRST = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(err, grpcx_pb::ImportError::Semantic(m) if m.contains("zero")));
}

#[test]
fn edition_2023_does_not_accept_optional_prefix() {
    // `optional` is proto3-translation-only; in 2023
    // sources presence is a feature, not a label.
    assert!(
        grpcx_pb::import(
            r#"edition = "2023";
message M { optional string x = 1; }
"#
        )
        .is_err()
    );
}

#[test]
fn proto3_field_named_optional_still_errors_cleanly() {
    // protoc has the same restriction: `optional` at field
    // position is always the modifier in proto3.
    assert!(
        grpcx_pb::import(
            r#"syntax = "proto3";
message M { string optional = 1; }
"#
        )
        .is_ok()
    );
    // …but as the FIRST token of a field decl it is
    // consumed as the modifier, so a bare `optional = 1`
    // is a parse error (matches protoc).
    assert!(
        grpcx_pb::import(
            r#"syntax = "proto3";
message M { optional = 1; }
"#
        )
        .is_err()
    );
}

#[test]
fn proto3_optional_repeated_and_optional_map_are_rejected() {
    let err = grpcx_pb::import(
        r#"syntax = "proto3";
message M { optional repeated int32 xs = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("optional repeated")
    ));
    let err = grpcx_pb::import(
        r#"syntax = "proto3";
message M { optional map<string, int32> m = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("optional map")
    ));
}

#[test]
fn optional_codegen_emits_option_fields() {
    let schema = grpcx_pb::import(
        r#"syntax = "proto3";
message P {
  optional int32 oi = 1;
  optional string os = 2;
  int32 plain = 3;
}
"#,
    )
    .unwrap();
    let out = grpcx_pb::codegen(&schema);
    assert!(out.contains("pub oi: Option<i32>"), "{out}");
    assert!(out.contains("pub os: Option<String>"), "{out}");
    assert!(out.contains("pub plain: i32"), "{out}");
    // Explicit presence: the encode path is Some-gated.
    assert!(out.contains("if let Some(__v) = &self.oi"), "{out}");
}

// ============================================================
// proto2 translation (DX stage C, 1.5) — labels onto the
// editions kernel: optional → Optional, required → Singular,
// repeated → Repeated; map fields and oneof members label-less.
// ============================================================

#[test]
fn proto2_full_file_translates_onto_2023_kernel() {
    let schema = grpcx_pb::import(
        r#"syntax = "proto2";
package legacy.v1;
message Item {
  required string sku = 1;
  optional string note = 2 [default = "n/a"];
  repeated int32 tags = 3;
  map<string, int32> attrs = 4;
  oneof body {
    string text = 5;
    bytes blob = 6;
  }
}
service Shop { rpc Get (Item) returns (Item); }
"#,
    )
    .unwrap();
    assert_eq!(schema.source_dialect, grpcx_pb::Dialect::Proto2);
    assert_eq!(schema.package.as_deref(), Some("legacy.v1"));
    let item = &schema.messages[0];
    assert_eq!(item.fields.len(), 6);
    // required → Singular (always-encoded, like editions
    // LEGACY_REQUIRED; no missing-required decode error).
    assert_eq!(item.fields[0].label, grpcx_pb::Label::Singular);
    // optional → Optional (the stage-B presence kernel);
    // `[default = …]` is parsed and discarded.
    assert_eq!(item.fields[1].label, grpcx_pb::Label::Optional);
    assert_eq!(item.fields[2].label, grpcx_pb::Label::Repeated);
    // map fields take no label in proto2.
    assert_eq!(item.fields[3].label, grpcx_pb::Label::Singular);
    // oneof members take no label either.
    assert_eq!(item.fields[4].name, "text");
    assert_eq!(item.fields[4].label, grpcx_pb::Label::Singular);
    assert_eq!(schema.services[0].methods[0].name, "Get");
}

#[test]
fn proto2_field_without_label_rejected() {
    let err = grpcx_pb::import(
        r#"syntax = "proto2";
message M { int32 x = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m)
            if m.contains("exactly one optional/required/repeated")
    ));
}

#[test]
fn proto2_double_label_and_labeled_map_rejected() {
    let err = grpcx_pb::import(
        r#"syntax = "proto2";
message M { required repeated int32 xs = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("at most one label")
    ));
    let err = grpcx_pb::import(
        r#"syntax = "proto2";
message M { required map<string, int32> m = 1; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("map fields take no label")
    ));
}

#[test]
fn proto2_group_and_extensions_rejected_by_name() {
    // Labeled group: the legacy delimited-message form.
    let err = grpcx_pb::import(
        r#"syntax = "proto2";
message M { optional group Result = 1 { optional int32 a = 2; } }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("group")
    ));
    let err = grpcx_pb::import(
        r#"syntax = "proto2";
message M { extensions 100 to 199; }
"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        grpcx_pb::ImportError::UnsupportedFeature(m) if m.contains("extensions")
    ));
}

#[test]
fn proto2_enum_first_value_may_be_nonzero() {
    // The first-value-zero constraint is proto3-only;
    // proto2 enums are closed and may start anywhere.
    let schema = grpcx_pb::import(
        r#"syntax = "proto2";
enum Legacy { FIRST = 5; SECOND = 9; }
"#,
    )
    .unwrap();
    assert_eq!(schema.enums[0].values[0].number, 5);
}

#[test]
fn proto2_codegen_emits_option_and_plain_fields() {
    let schema = grpcx_pb::import(
        r#"syntax = "proto2";
message P {
  optional int32 oi = 1;
  optional string os = 2;
  required int32 must = 3;
}
"#,
    )
    .unwrap();
    let out = grpcx_pb::codegen(&schema);
    assert!(out.contains("pub oi: Option<i32>"), "{out}");
    assert!(out.contains("pub os: Option<String>"), "{out}");
    // required → plain T, always-encoded (prost-parity API).
    assert!(out.contains("pub must: i32"), "{out}");
    assert!(out.contains("if let Some(__v) = &self.oi"), "{out}");
}
