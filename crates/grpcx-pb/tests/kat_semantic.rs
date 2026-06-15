//! v0.4 KAT — semantic analyser cases via the public
//! `import` entry.
//!
//! Each case feeds `.proto` editions 2023+ source through
//! `import` and asserts either the resolved-`Schema` shape
//! (named refs reclassified + FQN rewritten) or the expected
//! `ImportError::Semantic(&'static str)` reason.

use grpcx_pb::{FieldType, ImportError, import};

fn ok(src: &str) -> grpcx_pb::Schema {
    import(src).unwrap_or_else(|e| panic!("expected Ok(Schema) for {src:?}, got {e:?}"))
}

fn semantic_err(src: &str) -> &'static str {
    match import(src).expect_err("expected ImportError::Semantic") {
        ImportError::Semantic(reason) => reason,
        other => panic!("expected Semantic, got {other:?}"),
    }
}

// ============================================================
// 1. Enum ref reclassified to FieldType::Enum.
// ============================================================

#[test]
fn enum_ref_reclassified() {
    let s = ok(r#"
edition = "2023";
enum Color { RED = 0; GREEN = 1; }
message Painted {
  Color shade = 1;
}
"#);
    let f = &s.messages[0].fields[0];
    assert!(matches!(&f.ty, FieldType::Enum(n) if n == "Color"));
}

// ============================================================
// 2. Message ref stays FieldType::Message; name → FQN.
// ============================================================

#[test]
fn message_ref_stays_message_and_gets_fqn() {
    let s = ok(r#"
edition = "2023";
package foo.bar;
message Inner {}
message Outer {
  Inner i = 1;
}
"#);
    let outer = &s.messages[1];
    assert!(matches!(
        &outer.fields[0].ty,
        FieldType::Message(n) if n == "foo.bar.Inner"
    ));
}

// ============================================================
// 3. Unresolved name → Semantic("unresolved type reference").
// ============================================================

#[test]
fn unresolved_type_ref_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  DoesNotExist x = 1;
}
"#,
    );
    assert_eq!(reason, "unresolved type reference");
}

// ============================================================
// 4. Leading-`.` FQN resolves directly.
// ============================================================

#[test]
fn leading_dot_fqn_resolves() {
    let s = ok(r#"
edition = "2023";
enum Status { OK = 0; ERR = 1; }
message M {
  .Status st = 1;
}
"#);
    // Top-level `Status` has FQN "Status" (no package).
    assert!(matches!(
        &s.messages[0].fields[0].ty,
        FieldType::Enum(n) if n == "Status"
    ));
}

// ============================================================
// 5. Nested-scope resolution: bare name finds nested decl.
// ============================================================

#[test]
fn nested_scope_resolves_inner() {
    let s = ok(r#"
edition = "2023";
message Outer {
  message Inner {}
  Inner i = 1;
}
"#);
    let outer = &s.messages[0];
    assert!(matches!(
        &outer.fields[0].ty,
        FieldType::Message(n) if n == "Outer.Inner"
    ));
}

// ============================================================
// 6. Walk-up: from nested scope, find sibling top-level decl.
// ============================================================

#[test]
fn walk_up_to_sibling_top_level() {
    let s = ok(r#"
edition = "2023";
enum Color { RED = 0; }
message Outer {
  message Inner {
    Color paint = 1;
  }
}
"#);
    // Path: Outer.Inner → walks up → Outer.Color (no) →
    // (top) → Color (hit).
    let inner = &s.messages[0].nested_messages[0];
    assert!(matches!(
        &inner.fields[0].ty,
        FieldType::Enum(n) if n == "Color"
    ));
}

// ============================================================
// 7. Package-aware FQN rewrite.
// ============================================================

#[test]
fn package_aware_fqn() {
    let s = ok(r#"
edition = "2023";
package a.b.c;
message Foo {}
message Bar {
  Foo f = 1;
}
"#);
    let bar = &s.messages[1];
    assert!(matches!(
        &bar.fields[0].ty,
        FieldType::Message(n) if n == "a.b.c.Foo"
    ));
}

// ============================================================
// 8. Field number 0 → out-of-range.
// ============================================================

#[test]
fn field_number_zero_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 0;
}
"#,
    );
    assert_eq!(reason, "field number out of range (1..=536_870_911)");
}

// ============================================================
// 9. Field number > 536_870_911 → out-of-range.
// ============================================================

#[test]
fn field_number_too_large_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 536870912;
}
"#,
    );
    assert_eq!(reason, "field number out of range (1..=536_870_911)");
}

// ============================================================
// 10. Field number in 19000..=19999 → reserved-range.
// ============================================================

#[test]
fn field_number_reserved_range_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 19000;
}
"#,
    );
    assert_eq!(reason, "field number in reserved range 19000..=19999");

    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 19999;
}
"#,
    );
    assert_eq!(reason, "field number in reserved range 19000..=19999");
}

// ============================================================
// 11. Field number 18999 / 20000 / 536_870_911 → accepted
//      (boundary sanity).
// ============================================================

#[test]
fn field_number_boundary_accepted() {
    let _ = ok(r#"
edition = "2023";
message M {
  string a = 1;
  string b = 18999;
  string c = 20000;
  string d = 536870911;
}
"#);
}

// ============================================================
// 12. Duplicate field name → Semantic.
// ============================================================

#[test]
fn duplicate_field_name_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 1;
  int32  a = 2;
}
"#,
    );
    assert_eq!(reason, "duplicate field name in message");
}

// ============================================================
// 13. Duplicate field number → Semantic.
// ============================================================

#[test]
fn duplicate_field_number_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
message M {
  string a = 1;
  int32  b = 1;
}
"#,
    );
    assert_eq!(reason, "duplicate field number in message");
}

// ============================================================
// 14. Duplicate enum value name → Semantic.
// ============================================================

#[test]
fn duplicate_enum_value_name_errors() {
    let reason = semantic_err(
        r#"
edition = "2023";
enum E {
  A = 0;
  A = 1;
}
"#,
    );
    assert_eq!(reason, "duplicate enum value name in enum");
}

// ============================================================
// 15. map<K, V> with enum-typed value → V reclassified.
// ============================================================

#[test]
fn map_value_enum_reclassified() {
    let s = ok(r#"
edition = "2023";
enum Color { RED = 0; }
message M {
  map<string, Color> by_name = 1;
}
"#);
    match &s.messages[0].fields[0].ty {
        FieldType::Map { key, value } => {
            assert_eq!(**key, FieldType::String_);
            assert!(matches!(value.as_ref(), FieldType::Enum(n) if n == "Color"));
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

// ============================================================
// 16. Nested message: fields resolve relative to nested scope.
// ============================================================

#[test]
fn nested_message_fields_resolve_in_scope() {
    let s = ok(r#"
edition = "2023";
package p;
enum Color { RED = 0; }
message Outer {
  message Inner {
    Color tint = 1;
  }
  Color outer_tint = 1;
}
"#);
    let outer = &s.messages[0];
    assert!(matches!(
        &outer.fields[0].ty,
        FieldType::Enum(n) if n == "p.Color"
    ));
    let inner = &outer.nested_messages[0];
    assert!(matches!(
        &inner.fields[0].ty,
        FieldType::Enum(n) if n == "p.Color"
    ));
}

// ============================================================
// 17. Innermost-scope shadowing: nested decl beats outer.
// ============================================================

#[test]
fn innermost_scope_shadows_outer() {
    let s = ok(r#"
edition = "2023";
enum Color { RED = 0; }
message Outer {
  enum Color { BLUE = 0; }
  Color c = 1;
}
"#);
    let outer = &s.messages[0];
    // `Outer.Color` (the nested enum) shadows the top-level
    // `Color`.
    assert!(matches!(
        &outer.fields[0].ty,
        FieldType::Enum(n) if n == "Outer.Color"
    ));
}
