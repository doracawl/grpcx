//! RFC 7230 syntax primitives KAT — full token set
//! coverage, quoted-string with escapes, header-name +
//! token-list helpers.

use byte_cursor::Cursor;

#[test]
fn read_quoted_string_returns_empty_when_no_opening_quote() {
    // `read_quoted_string` is be-nice: if the cursor isn't
    // sitting on `"`, it returns an empty string (mirrors
    // `read_token`'s empty-on-mismatch shape). Lets the
    // caller use it inside larger grammars that have already
    // dispatched on the first byte.
    let mut cur = Cursor::from_str("not-a-quoted-string");
    assert_eq!(http1_syntax::read_quoted_string(&mut cur).unwrap(), "");
    // Cursor position unchanged.
    assert_eq!(cur.pos(), 0);
}
use http1_syntax::{
    SyntaxError, header_name_eq, is_token, is_token_char, parse_token_list, read_quoted_string,
    read_token, read_token_or_quoted, token_list_contains, unquote_value,
};

// ============================================================
// is_token_char — full RFC 7230 § 3.2.6 tchar set.
// ============================================================

#[test]
fn token_char_set_accept_all_punctuation() {
    for &b in b"!#$%&'*+-.^_`|~" {
        assert!(is_token_char(b), "{:?} should be token", b as char);
    }
}

#[test]
fn token_char_set_accept_alphanumeric() {
    for b in b'A'..=b'Z' {
        assert!(is_token_char(b));
    }
    for b in b'a'..=b'z' {
        assert!(is_token_char(b));
    }
    for b in b'0'..=b'9' {
        assert!(is_token_char(b));
    }
}

#[test]
fn token_char_set_reject_separator_chars() {
    // RFC 7230 § 3.2.6 separator chars.
    for &b in b"()<>@,;:\"\\/[]?={} \t" {
        assert!(!is_token_char(b), "{:?} should NOT be token", b as char);
    }
}

#[test]
fn token_char_set_reject_control_chars() {
    for b in 0u8..=0x1F {
        assert!(!is_token_char(b));
    }
    assert!(!is_token_char(0x7F)); // DEL
}

#[test]
fn token_char_set_reject_non_ascii() {
    for b in 0x80u8..=0xFF {
        assert!(!is_token_char(b));
    }
}

#[test]
fn is_token_rejects_empty() {
    assert!(!is_token(b""));
}

#[test]
fn is_token_rejects_with_separator() {
    assert!(!is_token(b"hello world"));
    assert!(!is_token(b"foo;bar"));
}

#[test]
fn is_token_accepts_valid() {
    assert!(is_token(b"permessage-deflate"));
    assert!(is_token(b"a"));
    assert!(is_token(b"X-Custom_Header"));
}

// ============================================================
// read_token.
// ============================================================

#[test]
fn read_token_basic() {
    let mut c = Cursor::from_str("permessage-deflate;");
    let span = read_token(&mut c);
    assert_eq!(span, b"permessage-deflate");
    assert_eq!(c.peek(), Some(b';'));
}

#[test]
fn read_token_empty_at_separator() {
    let mut c = Cursor::from_str(";abc");
    let span = read_token(&mut c);
    assert!(span.is_empty());
    assert_eq!(c.pos(), 0);
}

#[test]
fn read_token_to_end() {
    let mut c = Cursor::from_str("entire-input");
    let span = read_token(&mut c);
    assert_eq!(span, b"entire-input");
    assert!(c.at_end());
}

// ============================================================
// read_quoted_string.
// ============================================================

#[test]
fn quoted_string_basic() {
    let mut c = Cursor::from_str("\"hello\"after");
    let out = read_quoted_string(&mut c).unwrap();
    assert_eq!(out, "hello");
    assert_eq!(c.peek(), Some(b'a'));
}

#[test]
fn quoted_string_with_escape() {
    let mut c = Cursor::from_str("\"hi \\\"world\\\"\"");
    let out = read_quoted_string(&mut c).unwrap();
    assert_eq!(out, "hi \"world\"");
}

#[test]
fn quoted_string_with_backslash_escape() {
    let mut c = Cursor::from_str("\"a\\\\b\"");
    let out = read_quoted_string(&mut c).unwrap();
    assert_eq!(out, "a\\b");
}

#[test]
fn quoted_string_unterminated_errors() {
    let mut c = Cursor::from_str("\"oops");
    let err = read_quoted_string(&mut c).unwrap_err();
    assert_eq!(err, SyntaxError::UnterminatedQuotedString);
}

#[test]
fn quoted_string_trailing_backslash_errors() {
    let mut c = Cursor::from_str("\"oops\\");
    let err = read_quoted_string(&mut c).unwrap_err();
    assert_eq!(err, SyntaxError::BadEscape);
}

#[test]
fn quoted_string_empty() {
    let mut c = Cursor::from_str("\"\"after");
    let out = read_quoted_string(&mut c).unwrap();
    assert!(out.is_empty());
    assert_eq!(c.peek(), Some(b'a'));
}

#[test]
fn quoted_string_with_special_chars() {
    let mut c = Cursor::from_str("\"foo, bar; baz=\"");
    let out = read_quoted_string(&mut c).unwrap();
    assert_eq!(out, "foo, bar; baz=");
}

// ============================================================
// read_token_or_quoted.
// ============================================================

#[test]
fn token_or_quoted_picks_quoted() {
    let mut c = Cursor::from_str("\"hello world\"");
    let out = read_token_or_quoted(&mut c).unwrap();
    assert_eq!(out, "hello world");
}

#[test]
fn token_or_quoted_picks_token() {
    let mut c = Cursor::from_str("simple;more");
    let out = read_token_or_quoted(&mut c).unwrap();
    assert_eq!(out, "simple");
    assert_eq!(c.peek(), Some(b';'));
}

#[test]
fn token_or_quoted_empty_errors() {
    let mut c = Cursor::from_str(";");
    let err = read_token_or_quoted(&mut c).unwrap_err();
    assert_eq!(err, SyntaxError::EmptyToken);
}

// ============================================================
// header_name_eq.
// ============================================================

#[test]
fn header_name_case_insensitive() {
    assert!(header_name_eq("Upgrade", "upgrade"));
    assert!(header_name_eq("upgrade", "UPGRADE"));
    assert!(header_name_eq("Sec-WebSocket-Key", "sec-websocket-key"));
}

#[test]
fn header_name_case_insensitive_distinguishes_different_names() {
    assert!(!header_name_eq("Upgrade", "Connection"));
}

// ============================================================
// token_list_contains + parse_token_list.
// ============================================================

#[test]
fn token_list_contains_matches_case_insensitive() {
    assert!(token_list_contains("keep-alive, Upgrade", "upgrade"));
    assert!(token_list_contains("keep-alive, Upgrade", "Upgrade"));
    assert!(token_list_contains("Upgrade", "upgrade"));
}

#[test]
fn token_list_contains_does_not_substring_match() {
    // "upgrades" should NOT match "upgrade".
    assert!(!token_list_contains("upgrades", "upgrade"));
    // "newkeep-alive" should NOT match "keep-alive".
    assert!(!token_list_contains("newkeep-alive", "keep-alive"));
}

#[test]
fn token_list_contains_misses_absent() {
    assert!(!token_list_contains("keep-alive, close", "upgrade"));
}

#[test]
fn parse_token_list_trims_whitespace() {
    let v = parse_token_list("a, b ,  c ");
    assert_eq!(v, vec!["a", "b", "c"]);
}

#[test]
fn parse_token_list_drops_empty_entries() {
    let v = parse_token_list("a,, ,b,");
    assert_eq!(v, vec!["a", "b"]);
}

#[test]
fn parse_token_list_empty_input() {
    let v: Vec<&str> = parse_token_list("");
    assert!(v.is_empty());
}

// ============================================================
// unquote_value.
// ============================================================

#[test]
fn unquote_value_passes_through_bare() {
    assert_eq!(unquote_value("bare").unwrap(), "bare");
    assert_eq!(unquote_value("15").unwrap(), "15");
}

#[test]
fn unquote_value_unquotes_quoted() {
    assert_eq!(unquote_value("\"quoted\"").unwrap(), "quoted");
}

#[test]
fn unquote_value_handles_escape() {
    assert_eq!(unquote_value("\"a\\\"b\"").unwrap(), "a\"b");
}
