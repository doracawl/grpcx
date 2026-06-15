// Crate-private lexer for `.proto` editions 2023+ source.
//
// Pipeline: UTF-8 `&str` → `Vec<Spanned>`. Errors short-circuit
// with `ImportError::Lex { line, col }`; the syntax-line gate
// classifies the source dialect (editions 2023 / proto3 /
// proto2) and rejects unknown editions with
// `ImportError::UnsupportedFeature`.
//
// Token grammar + state-table summary live in
// `../GRAMMAR.md`. Out-of-scope at v0.2 (octal IntLit /
// `\u{}` string escapes / raw newlines inside string literals)
// is listed there.

use crate::ImportError;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use byte_cursor::Cursor;

// ============================================================
// Token model.
// ============================================================

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned {
    pub token: Token,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    Keyword(Keyword),
    ScalarType(ScalarType),
    Ident(String),
    IntLit(u64),
    FloatLit(f64),
    StringLit(String),
    Punct(Punct),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keyword {
    Edition,
    Package,
    Import,
    Option,
    Message,
    Enum,
    Oneof,
    Repeated,
    Map,
    Service,
    Rpc,
    Returns,
    Reserved,
    To,
    Max,
    True,
    False,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarType {
    Int32,
    Int64,
    Uint32,
    Uint64,
    Sint32,
    Sint64,
    Fixed32,
    Fixed64,
    Sfixed32,
    Sfixed64,
    Float,
    Double,
    Bool,
    StringTy,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Punct {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LAngle,
    RAngle,
    Semicolon,
    Comma,
    Equals,
    Dot,
    Colon,
    Minus,
    Plus,
}

// ============================================================
// Entry points.
// ============================================================

pub(crate) fn tokenize(source: &str) -> Result<(Vec<Spanned>, crate::Dialect), ImportError> {
    let tokens = raw_tokenize(source)?;
    let dialect = gate_syntax_line(&tokens)?;
    Ok((tokens, dialect))
}

pub(crate) fn raw_tokenize(source: &str) -> Result<Vec<Spanned>, ImportError> {
    let mut cur = Cursor::from_str(source);
    let mut tokens = Vec::new();
    loop {
        skip_trivia(&mut cur)?;
        let (line, col) = cur.here();
        let Some(b) = cur.peek() else { break };
        let token = lex_one(&mut cur, b, line, col)?;
        tokens.push(Spanned { token, line, col });
    }
    Ok(tokens)
}

fn lex_one(cur: &mut Cursor<'_>, b: u8, line: u32, col: u32) -> Result<Token, ImportError> {
    let punct = match b {
        b'{' => Some(Punct::LBrace),
        b'}' => Some(Punct::RBrace),
        b'(' => Some(Punct::LParen),
        b')' => Some(Punct::RParen),
        b'[' => Some(Punct::LBracket),
        b']' => Some(Punct::RBracket),
        b'<' => Some(Punct::LAngle),
        b'>' => Some(Punct::RAngle),
        b';' => Some(Punct::Semicolon),
        b',' => Some(Punct::Comma),
        b'=' => Some(Punct::Equals),
        b':' => Some(Punct::Colon),
        b'-' => Some(Punct::Minus),
        b'+' => Some(Punct::Plus),
        _ => None,
    };
    if let Some(p) = punct {
        cur.advance();
        return Ok(Token::Punct(p));
    }
    match b {
        b'.' => {
            if let Some(tok) = read_dot_or_float(cur, line, col)? {
                Ok(tok)
            } else {
                cur.advance();
                Ok(Token::Punct(Punct::Dot))
            }
        }
        b'"' | b'\'' => Ok(Token::StringLit(read_string(cur, line, col)?)),
        d if d.is_ascii_digit() => read_number(cur, line, col),
        i if is_ident_start(i) => Ok(classify_ident(read_ident(cur))),
        _ => Err(ImportError::Lex { line, col }),
    }
}

// ============================================================
// Syntax-line gate (editions 2023 + proto3/proto2 translation).
// ============================================================

fn gate_syntax_line(tokens: &[Spanned]) -> Result<crate::Dialect, ImportError> {
    let t0 = tokens.first().ok_or(ImportError::Lex { line: 1, col: 1 })?;

    let dialect = match &t0.token {
        Token::Ident(name) if name.as_str() == "syntax" => crate::Dialect::Proto3,
        Token::Keyword(Keyword::Edition) => crate::Dialect::Edition2023,
        _ => {
            return Err(ImportError::Lex {
                line: t0.line,
                col: t0.col,
            });
        }
    };

    let t1 = tokens.get(1).ok_or(ImportError::Lex {
        line: t0.line,
        col: t0.col,
    })?;
    if !matches!(t1.token, Token::Punct(Punct::Equals)) {
        return Err(ImportError::Lex {
            line: t1.line,
            col: t1.col,
        });
    }

    let t2 = tokens.get(2).ok_or(ImportError::Lex {
        line: t1.line,
        col: t1.col,
    })?;
    let dialect = match (&t2.token, dialect) {
        (Token::StringLit(s), crate::Dialect::Edition2023) if s.as_str() == "2023" => dialect,
        (Token::StringLit(s), crate::Dialect::Proto3) if s.as_str() == "proto3" => dialect,
        (Token::StringLit(s), crate::Dialect::Proto3) if s.as_str() == "proto2" => {
            crate::Dialect::Proto2
        }
        (Token::StringLit(_), _) => {
            return Err(ImportError::UnsupportedFeature(
                "edition must be \"2023\" — only editions 2023+ (currently 2023) supported",
            ));
        }
        _ => {
            return Err(ImportError::Lex {
                line: t2.line,
                col: t2.col,
            });
        }
    };

    let t3 = tokens.get(3).ok_or(ImportError::Lex {
        line: t2.line,
        col: t2.col,
    })?;
    if !matches!(t3.token, Token::Punct(Punct::Semicolon)) {
        return Err(ImportError::Lex {
            line: t3.line,
            col: t3.col,
        });
    }

    Ok(dialect)
}

// ============================================================
// Trivia: whitespace + line / block comments.
// ============================================================

fn skip_trivia(cur: &mut Cursor<'_>) -> Result<(), ImportError> {
    loop {
        match cur.peek() {
            None => return Ok(()),
            Some(b' ' | b'\t' | b'\r' | b'\n') => {
                cur.advance();
            }
            Some(b'/') => match cur.peek_at(1) {
                Some(b'/') => {
                    cur.advance();
                    cur.advance();
                    while let Some(b) = cur.peek() {
                        if b == b'\n' {
                            break;
                        }
                        cur.advance();
                    }
                }
                Some(b'*') => {
                    let (start_line, start_col) = cur.here();
                    cur.advance();
                    cur.advance();
                    loop {
                        match (cur.peek(), cur.peek_at(1)) {
                            (Some(b'*'), Some(b'/')) => {
                                cur.advance();
                                cur.advance();
                                break;
                            }
                            (None, _) => {
                                return Err(ImportError::Lex {
                                    line: start_line,
                                    col: start_col,
                                });
                            }
                            _ => {
                                cur.advance();
                            }
                        }
                    }
                }
                // Lone `/` is not trivia; main loop will Lex-reject.
                _ => return Ok(()),
            },
            Some(_) => return Ok(()),
        }
    }
}

// ============================================================
// Identifiers + keyword / scalar-type classification.
// ============================================================

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn read_ident(cur: &mut Cursor<'_>) -> String {
    let start = cur.pos();
    while let Some(b) = cur.peek() {
        if is_ident_continue(b) {
            cur.advance();
        } else {
            break;
        }
    }
    // Idents are ASCII-only by `is_ident_*`; UTF-8 validity is
    // trivial.
    core::str::from_utf8(&cur.source()[start..cur.pos()])
        .expect("ASCII-only ident slice")
        .to_string()
}

fn classify_ident(s: String) -> Token {
    use Keyword as K;
    use ScalarType as S;
    let kw = match s.as_str() {
        "edition" => Some(K::Edition),
        "package" => Some(K::Package),
        "import" => Some(K::Import),
        "option" => Some(K::Option),
        "message" => Some(K::Message),
        "enum" => Some(K::Enum),
        "oneof" => Some(K::Oneof),
        "repeated" => Some(K::Repeated),
        "map" => Some(K::Map),
        "service" => Some(K::Service),
        "rpc" => Some(K::Rpc),
        "returns" => Some(K::Returns),
        "reserved" => Some(K::Reserved),
        "to" => Some(K::To),
        "max" => Some(K::Max),
        "true" => Some(K::True),
        "false" => Some(K::False),
        _ => None,
    };
    if let Some(k) = kw {
        return Token::Keyword(k);
    }
    let scalar = match s.as_str() {
        "int32" => Some(S::Int32),
        "int64" => Some(S::Int64),
        "uint32" => Some(S::Uint32),
        "uint64" => Some(S::Uint64),
        "sint32" => Some(S::Sint32),
        "sint64" => Some(S::Sint64),
        "fixed32" => Some(S::Fixed32),
        "fixed64" => Some(S::Fixed64),
        "sfixed32" => Some(S::Sfixed32),
        "sfixed64" => Some(S::Sfixed64),
        "float" => Some(S::Float),
        "double" => Some(S::Double),
        "bool" => Some(S::Bool),
        "string" => Some(S::StringTy),
        "bytes" => Some(S::Bytes),
        _ => None,
    };
    if let Some(t) = scalar {
        return Token::ScalarType(t);
    }
    Token::Ident(s)
}

// ============================================================
// Numeric literals.
// ============================================================

fn read_number(cur: &mut Cursor<'_>, line: u32, col: u32) -> Result<Token, ImportError> {
    // Hex: `0x…` / `0X…`
    if cur.peek() == Some(b'0') && matches!(cur.peek_at(1), Some(b'x') | Some(b'X')) {
        cur.advance();
        cur.advance();
        let hex_start = cur.pos();
        while let Some(b) = cur.peek() {
            if b.is_ascii_hexdigit() {
                cur.advance();
            } else {
                break;
            }
        }
        if cur.pos() == hex_start {
            return Err(ImportError::Lex { line, col });
        }
        let s = core::str::from_utf8(&cur.source()[hex_start..cur.pos()])
            .expect("ASCII-only hex slice");
        let v = u64::from_str_radix(s, 16).map_err(|_| ImportError::Lex { line, col })?;
        return Ok(Token::IntLit(v));
    }

    let start = cur.pos();
    while let Some(b) = cur.peek() {
        if b.is_ascii_digit() {
            cur.advance();
        } else {
            break;
        }
    }
    let mut is_float = false;
    if cur.peek() == Some(b'.') && matches!(cur.peek_at(1), Some(d) if d.is_ascii_digit()) {
        is_float = true;
        cur.advance();
        while let Some(b) = cur.peek() {
            if b.is_ascii_digit() {
                cur.advance();
            } else {
                break;
            }
        }
    }
    if matches!(cur.peek(), Some(b'e' | b'E')) {
        is_float = true;
        cur.advance();
        if matches!(cur.peek(), Some(b'+' | b'-')) {
            cur.advance();
        }
        let exp_start = cur.pos();
        while let Some(b) = cur.peek() {
            if b.is_ascii_digit() {
                cur.advance();
            } else {
                break;
            }
        }
        if cur.pos() == exp_start {
            return Err(ImportError::Lex { line, col });
        }
    }
    let s =
        core::str::from_utf8(&cur.source()[start..cur.pos()]).expect("ASCII-only numeric slice");
    if is_float {
        let v: f64 = s.parse().map_err(|_| ImportError::Lex { line, col })?;
        Ok(Token::FloatLit(v))
    } else {
        let v: u64 = s.parse().map_err(|_| ImportError::Lex { line, col })?;
        Ok(Token::IntLit(v))
    }
}

fn read_dot_or_float(
    cur: &mut Cursor<'_>,
    line: u32,
    col: u32,
) -> Result<Option<Token>, ImportError> {
    if !matches!(cur.peek_at(1), Some(d) if d.is_ascii_digit()) {
        return Ok(None);
    }
    let start = cur.pos();
    cur.advance(); // consume `.`
    while let Some(b) = cur.peek() {
        if b.is_ascii_digit() {
            cur.advance();
        } else {
            break;
        }
    }
    if matches!(cur.peek(), Some(b'e' | b'E')) {
        cur.advance();
        if matches!(cur.peek(), Some(b'+' | b'-')) {
            cur.advance();
        }
        let exp_start = cur.pos();
        while let Some(b) = cur.peek() {
            if b.is_ascii_digit() {
                cur.advance();
            } else {
                break;
            }
        }
        if cur.pos() == exp_start {
            return Err(ImportError::Lex { line, col });
        }
    }
    let s = core::str::from_utf8(&cur.source()[start..cur.pos()]).expect("ASCII-only float slice");
    let v: f64 = s.parse().map_err(|_| ImportError::Lex { line, col })?;
    Ok(Some(Token::FloatLit(v)))
}

// ============================================================
// String literals.
// ============================================================

fn read_string(cur: &mut Cursor<'_>, line: u32, col: u32) -> Result<String, ImportError> {
    let quote = cur.advance().expect("caller verified opening quote");
    let mut s = String::new();
    loop {
        match cur.peek() {
            None | Some(b'\n') => return Err(ImportError::Lex { line, col }),
            Some(b) if b == quote => {
                cur.advance();
                return Ok(s);
            }
            Some(b'\\') => {
                cur.advance();
                let esc = cur.advance().ok_or(ImportError::Lex { line, col })?;
                let ch = match esc {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    b'\\' => '\\',
                    b'\'' => '\'',
                    b'"' => '"',
                    _ => return Err(ImportError::Lex { line, col }),
                };
                s.push(ch);
            }
            Some(b) => {
                cur.advance();
                if b < 0x80 {
                    s.push(b as char);
                } else {
                    let n = if b & 0xE0 == 0xC0 {
                        2
                    } else if b & 0xF0 == 0xE0 {
                        3
                    } else if b & 0xF8 == 0xF0 {
                        4
                    } else {
                        return Err(ImportError::Lex { line, col });
                    };
                    let mut buf = [0u8; 4];
                    buf[0] = b;
                    for byte in buf.iter_mut().take(n).skip(1) {
                        let cb = cur.advance().ok_or(ImportError::Lex { line, col })?;
                        if cb & 0xC0 != 0x80 {
                            return Err(ImportError::Lex { line, col });
                        }
                        *byte = cb;
                    }
                    let chunk = core::str::from_utf8(&buf[..n])
                        .map_err(|_| ImportError::Lex { line, col })?;
                    s.push_str(chunk);
                }
            }
        }
    }
}

// ============================================================
// Tests — token-class unit coverage via `raw_tokenize`.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn toks(src: &str) -> Vec<Token> {
        raw_tokenize(src)
            .unwrap()
            .into_iter()
            .map(|s| s.token)
            .collect()
    }

    fn spans(src: &str) -> Vec<(u32, u32)> {
        raw_tokenize(src)
            .unwrap()
            .into_iter()
            .map(|s| (s.line, s.col))
            .collect()
    }

    // ---------- Keywords + scalar types + ident split ---------- //

    #[test]
    fn classify_keyword_set() {
        let src = "edition package import option message enum oneof repeated map service rpc returns reserved to max true false";
        let expected: Vec<Token> = [
            Keyword::Edition,
            Keyword::Package,
            Keyword::Import,
            Keyword::Option,
            Keyword::Message,
            Keyword::Enum,
            Keyword::Oneof,
            Keyword::Repeated,
            Keyword::Map,
            Keyword::Service,
            Keyword::Rpc,
            Keyword::Returns,
            Keyword::Reserved,
            Keyword::To,
            Keyword::Max,
            Keyword::True,
            Keyword::False,
        ]
        .into_iter()
        .map(Token::Keyword)
        .collect();
        assert_eq!(toks(src), expected);
    }

    #[test]
    fn classify_scalar_type_set() {
        let src = "int32 int64 uint32 uint64 sint32 sint64 fixed32 fixed64 sfixed32 sfixed64 float double bool string bytes";
        let expected: Vec<Token> = [
            ScalarType::Int32,
            ScalarType::Int64,
            ScalarType::Uint32,
            ScalarType::Uint64,
            ScalarType::Sint32,
            ScalarType::Sint64,
            ScalarType::Fixed32,
            ScalarType::Fixed64,
            ScalarType::Sfixed32,
            ScalarType::Sfixed64,
            ScalarType::Float,
            ScalarType::Double,
            ScalarType::Bool,
            ScalarType::StringTy,
            ScalarType::Bytes,
        ]
        .into_iter()
        .map(Token::ScalarType)
        .collect();
        assert_eq!(toks(src), expected);
    }

    #[test]
    fn ident_distinct_from_keyword() {
        assert_eq!(
            toks("Foo bar _baz x1"),
            vec![
                Token::Ident("Foo".to_string()),
                Token::Ident("bar".to_string()),
                Token::Ident("_baz".to_string()),
                Token::Ident("x1".to_string()),
            ]
        );
        // `syntax` is NOT in the editions 2023+ keyword set —
        // lexes as an ident; the syntax-line gate detects it
        // separately.
        assert_eq!(toks("syntax"), vec![Token::Ident("syntax".to_string())]);
    }

    // ---------- Numeric literals ---------- //

    #[test]
    fn int_lit_decimal_and_hex() {
        assert_eq!(
            toks("0 1 42 12345"),
            vec![
                Token::IntLit(0),
                Token::IntLit(1),
                Token::IntLit(42),
                Token::IntLit(12345),
            ]
        );
        assert_eq!(
            toks("0x0 0xff 0xDEADBEEF 0X10"),
            vec![
                Token::IntLit(0),
                Token::IntLit(0xff),
                Token::IntLit(0xDEADBEEF),
                Token::IntLit(0x10),
            ]
        );
    }

    #[test]
    fn float_lit_forms() {
        assert_eq!(toks("1.5"), vec![Token::FloatLit(1.5)]);
        assert_eq!(toks("0.0"), vec![Token::FloatLit(0.0)]);
        assert_eq!(toks(".25"), vec![Token::FloatLit(0.25)]);
        assert_eq!(toks("1e10"), vec![Token::FloatLit(1e10)]);
        assert_eq!(toks("1.5e10"), vec![Token::FloatLit(1.5e10)]);
        assert_eq!(toks("1E-3"), vec![Token::FloatLit(1e-3)]);
        assert_eq!(toks("2.5e+2"), vec![Token::FloatLit(2.5e2)]);
    }

    #[test]
    fn int_followed_by_dot_punct() {
        // `1.foo` → IntLit(1), Dot, Ident("foo") — `.` not
        // followed by a digit is a Punct, not the start of a
        // float.
        assert_eq!(
            toks("1.foo"),
            vec![
                Token::IntLit(1),
                Token::Punct(Punct::Dot),
                Token::Ident("foo".to_string()),
            ]
        );
    }

    // ---------- String literals + escapes ---------- //

    #[test]
    fn string_double_and_single_quote() {
        assert_eq!(
            toks("\"hello\""),
            vec![Token::StringLit("hello".to_string()),]
        );
        assert_eq!(
            toks("'hello'"),
            vec![Token::StringLit("hello".to_string()),]
        );
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            toks(r#""a\nb\tc\rd\0e\\f\'g\"h""#),
            vec![Token::StringLit("a\nb\tc\rd\0e\\f'g\"h".to_string()),]
        );
    }

    #[test]
    fn string_utf8_passthrough() {
        // `é` = 2 UTF-8 bytes; `🦀` = 4 bytes. Both must round-
        // trip through `read_string`.
        let src = "\"héllo🦀\"";
        let mut t = raw_tokenize(src).unwrap().into_iter();
        let Token::StringLit(s) = t.next().unwrap().token else {
            panic!("expected StringLit");
        };
        assert_eq!(s, "héllo🦀");
    }

    #[test]
    fn string_unterminated_rejected() {
        let err = raw_tokenize("\"oops").unwrap_err();
        assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
    }

    #[test]
    fn string_raw_newline_rejected() {
        // A raw `\n` inside `"…"` is rejected (Rust-tokenizer
        // shape, not C-string shape).
        let err = raw_tokenize("\"oops\nstill\"").unwrap_err();
        assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
    }

    #[test]
    fn string_unknown_escape_rejected() {
        let err = raw_tokenize("\"\\q\"").unwrap_err();
        assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
    }

    // ---------- Punctuation ---------- //

    #[test]
    fn punct_full_set() {
        let src = "{ } ( ) [ ] < > ; , = . : - +";
        let expected: Vec<Token> = [
            Punct::LBrace,
            Punct::RBrace,
            Punct::LParen,
            Punct::RParen,
            Punct::LBracket,
            Punct::RBracket,
            Punct::LAngle,
            Punct::RAngle,
            Punct::Semicolon,
            Punct::Comma,
            Punct::Equals,
            Punct::Dot,
            Punct::Colon,
            Punct::Minus,
            Punct::Plus,
        ]
        .into_iter()
        .map(Token::Punct)
        .collect();
        assert_eq!(toks(src), expected);
    }

    // ---------- Trivia ---------- //

    #[test]
    fn line_comment_skipped() {
        assert_eq!(
            toks("foo // comment to eol\nbar"),
            vec![
                Token::Ident("foo".to_string()),
                Token::Ident("bar".to_string()),
            ]
        );
    }

    #[test]
    fn block_comment_skipped() {
        assert_eq!(
            toks("foo /* block\nmultiline */ bar"),
            vec![
                Token::Ident("foo".to_string()),
                Token::Ident("bar".to_string()),
            ]
        );
    }

    #[test]
    fn block_comment_unterminated_rejected() {
        let err = raw_tokenize("/* never closed").unwrap_err();
        assert!(matches!(err, ImportError::Lex { line: 1, col: 1 }));
    }

    #[test]
    fn lone_slash_rejected() {
        let err = raw_tokenize("foo / bar").unwrap_err();
        // Lex error at the `/` byte (col 5; "foo " is 4 cols
        // before it).
        assert!(matches!(err, ImportError::Lex { line: 1, col: 5 }));
    }

    // ---------- Line / col tracking ---------- //

    #[test]
    fn line_col_tracking() {
        let src = "foo bar\n  baz\nqux";
        assert_eq!(
            spans(src),
            vec![
                (1, 1), // foo
                (1, 5), // bar
                (2, 3), // baz
                (3, 1), // qux
            ]
        );
    }

    // ---------- Non-ASCII outside strings rejected ---------- //

    #[test]
    fn non_ascii_outside_string_rejected() {
        let err = raw_tokenize("foo é bar").unwrap_err();
        assert!(matches!(err, ImportError::Lex { .. }));
    }
}
