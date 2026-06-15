//! v0.2 KAT — syntax-line gate via the public `import` entry.
//!
//! These tests exercise the gate from the user's perspective:
//! `import(source: &str)` is the only public surface, so v0.2
//! observable behaviour is "did the gate accept editions 2023
//! and produce the expected sentinel error for v0.3, or did it
//! reject with the right `ImportError` variant?"
//!
//! Token-class coverage lives in `src/lexer.rs::tests` (unit
//! tests, direct against `raw_tokenize`); this file owns the
//! cross-boundary contract.

use grpcx_pb::{ImportError, import};

// ============================================================
// Accept path — editions 2023 source → `Ok(Schema)` via parser
//
// (v0.2 of this file expected an `UnsupportedFeature("parser
// lands v0.3")` sentinel; v0.3 wires the parser and returns
// the real `Schema`. These tests now assert the gate's accept
// path lets the parser see the body.)
// ============================================================

#[test]
fn edition_2023_accept_produces_schema() {
    let src = "edition = \"2023\";\nmessage Empty {}\n";
    let schema = import(src).expect("editions 2023 source should parse");
    assert_eq!(schema.messages.len(), 1);
    assert_eq!(schema.messages[0].name, "Empty");
}

#[test]
fn edition_2023_with_leading_comments_accepted() {
    let src = "// licence header\n/* multi\n   line */\nedition = \"2023\";\n";
    assert!(import(src).is_ok());
}

#[test]
fn edition_2023_single_quoted_accepted() {
    // protobuf string lits accept either `"..."` or `'...'`.
    let src = "edition = '2023';\n";
    assert!(import(src).is_ok());
}

// ============================================================
// Reject path — wrong edition / missing syntax line
// ============================================================

#[test]
fn proto2_syntax_accepted_since_1_5() {
    // DX stage C: proto2 sources translate onto the 2023
    // kernel at the frontend, same as proto3.
    let schema = import("syntax = \"proto2\";\nmessage Foo { optional int32 a = 1; }\n").unwrap();
    assert_eq!(schema.source_dialect, grpcx_pb::Dialect::Proto2);
    assert_eq!(schema.messages.len(), 1);
}

#[test]
fn proto3_syntax_accepted_since_1_3() {
    // DX stage A: proto3 sources translate onto the 2023
    // kernel at the frontend.
    let schema = import("syntax = \"proto3\";\nmessage Foo {}\n").unwrap();
    assert_eq!(schema.source_dialect, grpcx_pb::Dialect::Proto3);
    assert_eq!(schema.messages.len(), 1);
}

#[test]
fn wrong_edition_rejected() {
    let err = import("edition = \"1999\";\n").unwrap_err();
    let ImportError::UnsupportedFeature(msg) = err else {
        panic!("expected UnsupportedFeature, got {err:?}");
    };
    assert!(
        msg.contains("edition must be \"2023\""),
        "expected edition message, got {msg:?}"
    );
}

#[test]
fn future_edition_rejected_until_supported() {
    // Future editions land in lib.rs's `Edition` enum + the
    // gate's allowlist additively; for v0.2 only "2023" is
    // accepted.
    let err = import("edition = \"2024\";\n").unwrap_err();
    assert!(matches!(err, ImportError::UnsupportedFeature(_)));
}

#[test]
fn missing_edition_line_rejected_as_lex() {
    // No `edition = "2023";` line → gate's "first token must
    // be `edition` keyword" arm fires; reported at the
    // offending first token's span.
    let err = import("message Foo {}\n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
}

#[test]
fn empty_source_rejected_as_lex() {
    let err = import("").unwrap_err();
    assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
}

#[test]
fn only_whitespace_rejected_as_lex() {
    // Trivia-only input still has no syntax line; the gate's
    // "empty token stream" arm fires.
    let err = import("   \n\n\t  \n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
}

#[test]
fn only_comments_rejected_as_lex() {
    // Same as above — comments alone are trivia.
    let err = import("// just a header\n/* and a banner */\n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
}

// ============================================================
// Lex errors pass through `import`
// ============================================================

#[test]
fn lex_error_propagates_with_position() {
    // Unterminated string mid-file. The edition line lexes
    // clean; the lex error fires at the opening quote of the
    // bad string on line 2.
    let src = "edition = \"2023\";\noption foo = \"oops\n";
    let err = import(src).unwrap_err();
    match err {
        ImportError::Lex { line: 2, col } => {
            // Opening quote is at col 14 of line 2:
            //   `option foo = "oops`
            //    123456789012345
            assert_eq!(col, 14);
        }
        other => panic!("expected Lex {{ line: 2, col: 14 }}, got {other:?}"),
    }
}

#[test]
fn lex_error_unterminated_block_comment() {
    let src = "edition = \"2023\";\n/* never closed";
    let err = import(src).unwrap_err();
    assert!(matches!(err, ImportError::Lex { line: 2, col: 1 }));
}

// ============================================================
// Gate edge — partial syntax lines
// ============================================================

#[test]
fn edition_without_equals_rejected_as_lex() {
    // `edition "2023";` — gate's "expect `=` after `edition`"
    // arm fires at the offending `"2023"` literal.
    let err = import("edition \"2023\";\n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { .. }));
}

#[test]
fn edition_without_string_rejected_as_lex() {
    // `edition = 2023;` — gate expects a StringLit at the
    // edition value slot; numeric literal triggers Lex.
    let err = import("edition = 2023;\n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { .. }));
}

#[test]
fn edition_without_semicolon_rejected_as_lex() {
    let err = import("edition = \"2023\"\n").unwrap_err();
    assert!(matches!(err, ImportError::Lex { .. }));
}

#[test]
fn truncated_edition_line_rejected_as_lex() {
    // Just `edition` and EOF.
    let err = import("edition").unwrap_err();
    assert!(matches!(err, ImportError::Lex { .. }));
}
