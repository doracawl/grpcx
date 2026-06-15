#![doc = include_str!("../README.md")]
#![no_std]

//! RFC 7230 HTTP/1.1 syntax primitives. See the README for
//! the feature list and design rationale.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use byte_cursor::Cursor;

// ============================================================
// Errors.
// ============================================================

/// Parse failure modes for the quoted-string + unquote
/// helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxError {
    /// Input ended mid quoted-string (missing closing `"`).
    UnterminatedQuotedString,
    /// `\` at end of input or before disallowed character.
    BadEscape,
    /// Expected a token character but found something
    /// else (e.g. `read_token` on empty input).
    EmptyToken,
}

// ============================================================
// Token character set (RFC 7230 § 3.2.6).
// ============================================================

/// `true` if `b` is in the RFC 7230 § 3.2.6 `tchar`
/// production:
///
/// ```text
/// tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*"
///       / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~"
///       / DIGIT / ALPHA
/// ```
pub const fn is_token_char(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
    )
}

/// `true` if `s` is a non-empty `token` per RFC 7230
/// § 3.2.6 (every byte is a `tchar`).
pub fn is_token(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&b| is_token_char(b))
}

// ============================================================
// Cursor-based primitives.
// ============================================================

/// Read a token from `cursor` (one or more `tchar` bytes).
/// Advances `cursor` past the token; returns the
/// borrowed slice.
///
/// Returns an empty slice if the cursor is at a
/// non-token byte — caller decides whether that's an
/// error.
pub fn read_token<'src>(cursor: &mut Cursor<'src>) -> &'src [u8] {
    cursor.take_while(is_token_char)
}

/// Read a quoted-string from `cursor`. Caller must have
/// already positioned the cursor at the opening `"`.
///
/// Handles `quoted-pair` (RFC 7230 § 3.2.6): `\` followed
/// by any byte produces the byte literally. Returns the
/// unquoted payload (without the surrounding `"`s, with
/// escapes resolved).
///
/// # Errors
/// - [`SyntaxError::UnterminatedQuotedString`] — input
///   ended before the closing `"`.
/// - [`SyntaxError::BadEscape`] — `\` at end of input.
pub fn read_quoted_string(cursor: &mut Cursor<'_>) -> Result<String, SyntaxError> {
    // Caller verified opening quote; consume it.
    if !cursor.try_consume(b'"') {
        // We were lenient — if there's no opening quote
        // this isn't a quoted string. Return empty.
        // Strict spec says "you should know what you're
        // calling"; this matches `read_token`'s be-nice
        // shape.
        return Ok(String::new());
    }
    let mut out = String::new();
    loop {
        match cursor.advance() {
            None => return Err(SyntaxError::UnterminatedQuotedString),
            Some(b'"') => return Ok(out),
            Some(b'\\') => {
                let next = cursor.advance().ok_or(SyntaxError::BadEscape)?;
                out.push(next as char);
            }
            Some(b) => out.push(b as char),
        }
    }
}

/// Parse a value that may be either a bare token or a
/// quoted-string. Caller-positioned cursor.
///
/// # Errors
/// - [`SyntaxError::EmptyToken`] — neither a token char
///   nor a `"` was found.
/// - Any error from [`read_quoted_string`].
pub fn read_token_or_quoted(cursor: &mut Cursor<'_>) -> Result<String, SyntaxError> {
    match cursor.peek() {
        Some(b'"') => read_quoted_string(cursor),
        Some(b) if is_token_char(b) => {
            let span = read_token(cursor);
            // `span` is ASCII tokens by construction →
            // valid UTF-8.
            Ok(core::str::from_utf8(span).unwrap().into())
        }
        _ => Err(SyntaxError::EmptyToken),
    }
}

// ============================================================
// String-level helpers (no cursor required).
// ============================================================

/// Case-insensitive ASCII comparison of two header names
/// (RFC 7230 § 3.2 — header names are case-insensitive).
///
/// Non-ASCII bytes compare by exact value (this matches
/// what HTTP libraries do for the rare case of UTF-8 in
/// names; it's not spec-compliant but interop-safe).
pub fn header_name_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Check whether `value` (a comma-separated list, per
/// RFC 7230 § 7 list semantics) contains `token` as one
/// of its elements. Token comparison is case-insensitive
/// ASCII.
///
/// Example:
///
/// ```ignore
/// assert!(token_list_contains("keep-alive, Upgrade", "upgrade"));
/// ```
pub fn token_list_contains(value: &str, token: &str) -> bool {
    parse_token_list(value)
        .iter()
        .any(|t| t.eq_ignore_ascii_case(token))
}

/// Split a comma-separated header value into its tokens.
/// Each returned token has surrounding whitespace
/// trimmed. Empty entries (from trailing commas, etc.)
/// are dropped.
pub fn parse_token_list(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect()
}

/// If `value` starts with `"`, parse as quoted-string;
/// otherwise return as-is. Convenience helper for
/// HTTP headers that allow either form (e.g.
/// `Sec-WebSocket-Extensions` param values).
///
/// # Errors
/// Forwarded from [`read_quoted_string`].
pub fn unquote_value(value: &str) -> Result<String, SyntaxError> {
    if value.starts_with('"') {
        let mut cursor = Cursor::from_str(value);
        read_quoted_string(&mut cursor)
    } else {
        Ok(value.into())
    }
}
