//! `byte-cursor` integration tests covering every public
//! method + line/col tracking + edge cases.

use byte_cursor::Cursor;

#[test]
fn source_returns_full_underlying_slice() {
    let buf = b"hello, world";
    let mut cur = Cursor::new(buf);
    cur.advance();
    cur.advance();
    // `source()` doesn't shift with the cursor — it always
    // returns the original input slice, lifetime-bound to
    // `'src`.
    assert_eq!(cur.source(), buf);
    assert_eq!(cur.pos(), 2);
}

#[test]
fn source_supports_slice_back_to_start_pattern() {
    let s = "abc def";
    let mut cur = Cursor::from_str(s);
    let start = cur.pos();
    cur.advance();
    cur.advance();
    cur.advance();
    assert_eq!(&cur.source()[start..cur.pos()], b"abc");
}

#[test]
fn expect_byte_consumes_match() {
    let mut cur = Cursor::new(b"=42");
    assert!(cur.expect_byte(b'='));
    assert_eq!(cur.pos(), 1);
    assert_eq!(cur.peek(), Some(b'4'));
}

#[test]
fn expect_byte_does_not_consume_mismatch() {
    let mut cur = Cursor::new(b"x42");
    assert!(!cur.expect_byte(b'='));
    assert_eq!(cur.pos(), 0);
    assert_eq!(cur.peek(), Some(b'x'));
}

// ============================================================
// Basic construction + position queries.
// ============================================================

#[test]
fn new_starts_at_origin() {
    let c = Cursor::new(b"abc");
    assert_eq!(c.pos(), 0);
    assert_eq!(c.here(), (1, 1));
    assert!(!c.at_end());
    assert_eq!(c.remaining(), b"abc");
}

#[test]
fn from_str_uses_utf8_bytes() {
    let c = Cursor::from_str("héllo");
    // `é` is 2 bytes in UTF-8.
    assert_eq!(c.remaining().len(), 6);
    assert_eq!(c.peek(), Some(b'h'));
}

#[test]
fn empty_input_is_at_end() {
    let c = Cursor::new(b"");
    assert!(c.at_end());
    assert_eq!(c.peek(), None);
}

// ============================================================
// peek / peek_at / advance.
// ============================================================

#[test]
fn peek_does_not_advance() {
    let c = Cursor::new(b"abc");
    assert_eq!(c.peek(), Some(b'a'));
    assert_eq!(c.peek(), Some(b'a')); // still a
    assert_eq!(c.pos(), 0);
}

#[test]
fn peek_at_offset() {
    let c = Cursor::new(b"abc");
    assert_eq!(c.peek_at(0), Some(b'a'));
    assert_eq!(c.peek_at(1), Some(b'b'));
    assert_eq!(c.peek_at(2), Some(b'c'));
    assert_eq!(c.peek_at(3), None);
    assert_eq!(c.peek_at(99), None);
}

#[test]
fn advance_returns_byte_and_moves_forward() {
    let mut c = Cursor::new(b"abc");
    assert_eq!(c.advance(), Some(b'a'));
    assert_eq!(c.pos(), 1);
    assert_eq!(c.peek(), Some(b'b'));
    assert_eq!(c.advance(), Some(b'b'));
    assert_eq!(c.advance(), Some(b'c'));
    assert_eq!(c.advance(), None);
    assert!(c.at_end());
}

// ============================================================
// Line / col tracking.
// ============================================================

#[test]
fn line_col_increment_on_normal_bytes() {
    let mut c = Cursor::new(b"abc");
    c.advance();
    assert_eq!(c.here(), (1, 2));
    c.advance();
    assert_eq!(c.here(), (1, 3));
}

#[test]
fn newline_resets_col_increments_line() {
    let mut c = Cursor::new(b"a\nb");
    c.advance(); // 'a' → (1, 2)
    c.advance(); // '\n' → (2, 1)
    assert_eq!(c.here(), (2, 1));
    c.advance(); // 'b' → (2, 2)
    assert_eq!(c.here(), (2, 2));
}

#[test]
fn multiple_newlines() {
    let mut c = Cursor::new(b"\n\n\nx");
    c.advance();
    assert_eq!(c.line(), 2);
    c.advance();
    assert_eq!(c.line(), 3);
    c.advance();
    assert_eq!(c.line(), 4);
    assert_eq!(c.col(), 1);
    c.advance();
    assert_eq!(c.here(), (4, 2));
}

// ============================================================
// try_consume / expect_byte.
// ============================================================

#[test]
fn try_consume_matches() {
    let mut c = Cursor::new(b"foo");
    assert!(c.try_consume(b'f'));
    assert_eq!(c.pos(), 1);
}

#[test]
fn try_consume_no_match_does_not_advance() {
    let mut c = Cursor::new(b"foo");
    assert!(!c.try_consume(b'x'));
    assert_eq!(c.pos(), 0);
}

#[test]
fn try_consume_at_end_returns_false() {
    let mut c = Cursor::new(b"");
    assert!(!c.try_consume(b'a'));
}

// ============================================================
// take_while.
// ============================================================

#[test]
fn take_while_ascii_alphanumeric() {
    let mut c = Cursor::new(b"hello123 world");
    let span = c.take_while(|b| b.is_ascii_alphanumeric());
    assert_eq!(span, b"hello123");
    assert_eq!(c.peek(), Some(b' '));
}

#[test]
fn take_while_no_match_returns_empty() {
    let mut c = Cursor::new(b"xyz");
    let span = c.take_while(|b| b.is_ascii_digit());
    assert!(span.is_empty());
    assert_eq!(c.pos(), 0);
}

#[test]
fn take_while_to_end_of_input() {
    let mut c = Cursor::new(b"abcdef");
    let span = c.take_while(|b| b.is_ascii_alphabetic());
    assert_eq!(span, b"abcdef");
    assert!(c.at_end());
}

#[test]
fn take_while_updates_line_col_on_newline() {
    let mut c = Cursor::new(b"line1\nline2\nrest");
    let span = c.take_while(|b| b != b'r');
    assert_eq!(span, b"line1\nline2\n");
    assert_eq!(c.here(), (3, 1));
}

// ============================================================
// skip_while.
// ============================================================

#[test]
fn skip_while_returns_count() {
    let mut c = Cursor::new(b"   hello");
    let n = c.skip_while(|b| b == b' ');
    assert_eq!(n, 3);
    assert_eq!(c.peek(), Some(b'h'));
}

#[test]
fn skip_while_no_match_returns_zero() {
    let mut c = Cursor::new(b"hello");
    let n = c.skip_while(|b| b == b' ');
    assert_eq!(n, 0);
}

// ============================================================
// Whitespace shorthands.
// ============================================================

#[test]
fn skip_ws_horizontal_only_spaces_and_tabs() {
    let mut c = Cursor::new(b" \t\n hello");
    let n = c.skip_ws_horizontal();
    assert_eq!(n, 2); // ` ` and `\t`
    assert_eq!(c.peek(), Some(b'\n'));
}

#[test]
fn skip_ws_includes_newlines() {
    let mut c = Cursor::new(b" \t\r\n hello");
    let n = c.skip_ws();
    assert_eq!(n, 5); // ` \t\r\n ` all consumed
    assert_eq!(c.peek(), Some(b'h'));
    // Line tracking: 1 `\n` was consumed → line 2.
    assert_eq!(c.line(), 2);
}

// ============================================================
// Composition smoke — parser-shape micro-test.
// ============================================================

#[test]
fn tokenize_key_equals_value() {
    // Pattern that mimics both lexer/parser uses: skip ws,
    // take_while(alnum), expect `=`, take_while(alnum).
    let mut c = Cursor::new(b"  key123 = value456");
    c.skip_ws_horizontal();
    let key = c.take_while(|b| b.is_ascii_alphanumeric());
    assert_eq!(key, b"key123");
    c.skip_ws_horizontal();
    assert!(c.try_consume(b'='));
    c.skip_ws_horizontal();
    let value = c.take_while(|b| b.is_ascii_alphanumeric());
    assert_eq!(value, b"value456");
    assert!(c.at_end());
}
