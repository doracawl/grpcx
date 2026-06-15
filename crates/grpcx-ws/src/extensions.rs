// RFC 6455 § 9.1 `Sec-WebSocket-Extensions` parser +
// formatter + RFC 7692 `permessage-deflate` recognition.
// Negotiation surface only — real compression is v1.x.
//
// See `../../REFACTOR-v9-v0.6-extensions.md` for design
// notes + the grammar excerpt.

use crate::WsError;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use byte_cursor::Cursor;
use http1_syntax::{SyntaxError, is_token_char, read_quoted_string, read_token};

// ============================================================
// Public ADTs.
// ============================================================

/// A single extension offer per RFC 6455 § 9.1:
/// `extension-token *( ";" extension-param )`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtensionOffer {
    /// Extension token (case-sensitive; e.g.
    /// `"permessage-deflate"`).
    pub name: String,
    /// Parameters: `(key, value)` for `key=value`,
    /// `(key, None)` for bare-key parameters.
    pub params: Vec<(String, Option<String>)>,
}

/// Strongly-typed `permessage-deflate` offer (RFC 7692
/// § 7.1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PermessageDeflateOffer {
    /// `server_no_context_takeover` — server disables
    /// LZ77 sliding window context carry-over between
    /// messages.
    pub server_no_context_takeover: bool,
    /// `client_no_context_takeover` — same for client.
    pub client_no_context_takeover: bool,
    /// `server_max_window_bits` — server's sliding-window
    /// size hint (8..=15).
    pub server_max_window_bits: Option<u8>,
    /// `client_max_window_bits` — client's sliding-window
    /// size hint (8..=15).
    pub client_max_window_bits: Option<u8>,
}

// ============================================================
// Public functions.
// ============================================================

/// Parse a full `Sec-WebSocket-Extensions` header value
/// (comma-separated offers).
pub fn parse_extension_header(value: &str) -> Result<Vec<ExtensionOffer>, WsError> {
    let mut cur = Cursor::from_str(value);
    let mut out = Vec::new();
    cur.skip_ws_horizontal();
    if cur.at_end() {
        return Ok(out);
    }
    loop {
        let offer = parse_one_offer(&mut cur)?;
        out.push(offer);
        cur.skip_ws_horizontal();
        match cur.peek() {
            None => return Ok(out),
            Some(b',') => {
                cur.advance();
                cur.skip_ws_horizontal();
                if cur.at_end() {
                    return Err(WsError::BadHandshake("extension header has trailing comma"));
                }
            }
            Some(_) => {
                return Err(WsError::BadHandshake(
                    "extension header has stray character after offer",
                ));
            }
        }
    }
}

/// Parse a single extension offer (no top-level commas).
pub fn parse_extension_offer(value: &str) -> Result<ExtensionOffer, WsError> {
    let mut cur = Cursor::from_str(value);
    cur.skip_ws_horizontal();
    let offer = parse_one_offer(&mut cur)?;
    cur.skip_ws_horizontal();
    if !cur.at_end() {
        return Err(WsError::BadHandshake(
            "extra characters after extension offer",
        ));
    }
    Ok(offer)
}

/// Emit a `Sec-WebSocket-Extensions` header value from a
/// list of offers. Symmetric inverse of
/// [`parse_extension_header`].
pub fn format_extension_header(offers: &[ExtensionOffer]) -> String {
    let mut out = String::new();
    for (i, offer) in offers.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&offer.name);
        for (k, v) in &offer.params {
            out.push_str("; ");
            out.push_str(k);
            if let Some(value) = v {
                out.push('=');
                if needs_quoting(value) {
                    out.push('"');
                    for c in value.chars() {
                        if c == '"' || c == '\\' {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                    out.push('"');
                } else {
                    out.push_str(value);
                }
            }
        }
    }
    out
}

/// Case-sensitive lookup. Extension tokens are case-
/// sensitive per RFC 6455 § 9.1 (they reference IANA
/// registrations which are exact-match).
pub fn find_offer_by_name<'a>(
    offers: &'a [ExtensionOffer],
    name: &str,
) -> Option<&'a ExtensionOffer> {
    offers.iter().find(|o| o.name == name)
}

impl PermessageDeflateOffer {
    /// Parse a `permessage-deflate` offer's parameters
    /// into a typed struct.
    ///
    /// # Errors
    /// - [`WsError::BadHandshake`] — offer name isn't
    ///   `"permessage-deflate"`, parameter window-bits is
    ///   out of range (must be 8..=15), or an unknown
    ///   parameter key appears.
    pub fn parse(offer: &ExtensionOffer) -> Result<Self, WsError> {
        if offer.name != "permessage-deflate" {
            return Err(WsError::BadHandshake(
                "offer name is not permessage-deflate",
            ));
        }
        let mut out = PermessageDeflateOffer::default();
        for (k, v) in &offer.params {
            match (k.as_str(), v.as_deref()) {
                ("server_no_context_takeover", None) => {
                    out.server_no_context_takeover = true;
                }
                ("client_no_context_takeover", None) => {
                    out.client_no_context_takeover = true;
                }
                ("server_max_window_bits", Some(val)) => {
                    out.server_max_window_bits = Some(parse_window_bits(val)?);
                }
                ("client_max_window_bits", Some(val)) => {
                    out.client_max_window_bits = Some(parse_window_bits(val)?);
                }
                ("client_max_window_bits", None) => {
                    // Client offer with no value is allowed
                    // per RFC 7692 § 7.1.2.2 — caller may
                    // still constrain. We record as None
                    // (i.e. "present but unspecified" is
                    // equivalent to "not present" for v0.6
                    // simplicity; v1.x revisits).
                }
                _ => {
                    return Err(WsError::BadHandshake(
                        "unknown or malformed permessage-deflate parameter",
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Render this struct back to an [`ExtensionOffer`].
    pub fn to_offer(&self) -> ExtensionOffer {
        let mut params = Vec::new();
        if self.server_no_context_takeover {
            params.push(("server_no_context_takeover".to_string(), None));
        }
        if self.client_no_context_takeover {
            params.push(("client_no_context_takeover".to_string(), None));
        }
        if let Some(n) = self.server_max_window_bits {
            params.push(("server_max_window_bits".to_string(), Some(int_to_str(n))));
        }
        if let Some(n) = self.client_max_window_bits {
            params.push(("client_max_window_bits".to_string(), Some(int_to_str(n))));
        }
        ExtensionOffer {
            name: "permessage-deflate".to_string(),
            params,
        }
    }
}

fn parse_window_bits(s: &str) -> Result<u8, WsError> {
    let n: u8 = s
        .parse()
        .map_err(|_| WsError::BadHandshake("invalid window bits value"))?;
    if !(8..=15).contains(&n) {
        return Err(WsError::BadHandshake("window bits must be in 8..=15"));
    }
    Ok(n)
}

fn int_to_str(n: u8) -> String {
    let mut s = String::new();
    // u8 max is 255 (3 chars); 8..=15 needs 1-2.
    if n >= 100 {
        s.push((b'0' + n / 100) as char);
    }
    if n >= 10 {
        s.push((b'0' + (n / 10) % 10) as char);
    }
    s.push((b'0' + n % 10) as char);
    s
}

// ============================================================
// Parser internals.
// ============================================================

fn parse_one_offer(cur: &mut Cursor<'_>) -> Result<ExtensionOffer, WsError> {
    cur.skip_ws_horizontal();
    let name = take_token(cur);
    if name.is_empty() {
        return Err(WsError::BadHandshake("extension offer missing name"));
    }
    let mut params = Vec::new();
    loop {
        cur.skip_ws_horizontal();
        match cur.peek() {
            Some(b';') => {
                cur.advance();
                cur.skip_ws_horizontal();
                let key = take_token(cur);
                if key.is_empty() {
                    return Err(WsError::BadHandshake("parameter key is empty"));
                }
                cur.skip_ws_horizontal();
                let value = if cur.peek() == Some(b'=') {
                    cur.advance();
                    cur.skip_ws_horizontal();
                    let v = if cur.peek() == Some(b'"') {
                        read_quoted_string(cur).map_err(quoted_err)?
                    } else {
                        let t = take_token(cur);
                        if t.is_empty() {
                            return Err(WsError::BadHandshake("parameter value is empty"));
                        }
                        t
                    };
                    Some(v)
                } else {
                    None
                };
                params.push((key, value));
            }
            _ => break,
        }
    }
    Ok(ExtensionOffer { name, params })
}

// Convenience wrapper: lift `&[u8]` from http1-syntax's
// `read_token` into an owned `String`. Token chars are
// pure ASCII so the from_utf8 cast is infallible.
fn take_token(cur: &mut Cursor<'_>) -> String {
    let bytes = read_token(cur);
    // SAFETY-equivalent argument: `read_token` only consumes
    // bytes for which `is_token_char` is true; that set is a
    // subset of ASCII, so the slice is always valid UTF-8.
    core::str::from_utf8(bytes)
        .expect("token bytes are ASCII")
        .to_string()
}

fn quoted_err(e: SyntaxError) -> WsError {
    match e {
        SyntaxError::UnterminatedQuotedString => {
            WsError::BadHandshake("unterminated quoted string in extension header")
        }
        SyntaxError::BadEscape => WsError::BadHandshake("trailing backslash in quoted string"),
        SyntaxError::EmptyToken => WsError::BadHandshake("expected quoted string"),
    }
}

fn needs_quoting(s: &str) -> bool {
    s.is_empty() || s.bytes().any(|b| !is_token_char(b))
}

// ============================================================
// Unit tests — primitives. Public-surface coverage lives in
// `tests/kat_extensions.rs`.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_ws_smoke() {
        let mut c = Cursor::from_str("   abc");
        c.skip_ws_horizontal();
        assert_eq!(c.peek(), Some(b'a'));
    }

    #[test]
    fn read_token_basic() {
        let mut c = Cursor::from_str("hello,world");
        assert_eq!(take_token(&mut c), "hello");
        assert_eq!(c.peek(), Some(b','));
    }

    #[test]
    fn read_token_with_punct() {
        let mut c = Cursor::from_str("permessage-deflate;");
        assert_eq!(take_token(&mut c), "permessage-deflate");
    }

    #[test]
    fn read_quoted_with_escape() {
        let mut c = Cursor::from_str("\"he\\\"llo\"after");
        assert_eq!(read_quoted_string(&mut c).unwrap(), "he\"llo");
        assert_eq!(c.peek(), Some(b'a'));
    }

    #[test]
    fn read_quoted_unterminated() {
        let mut c = Cursor::from_str("\"oops");
        assert!(read_quoted_string(&mut c).is_err());
    }

    #[test]
    fn needs_quoting_decisions() {
        assert!(!needs_quoting("simple"));
        assert!(needs_quoting(""));
        assert!(needs_quoting("with space"));
        assert!(needs_quoting("with,comma"));
    }

    #[test]
    fn int_to_str_smoke() {
        assert_eq!(int_to_str(8), "8");
        assert_eq!(int_to_str(15), "15");
        assert_eq!(int_to_str(100), "100");
    }
}
