# http1-syntax

[![crates.io](https://img.shields.io/crates/v/http1-syntax.svg)](https://crates.io/crates/http1-syntax)
[![docs.rs](https://docs.rs/http1-syntax/badge.svg)](https://docs.rs/http1-syntax)
[![license](https://img.shields.io/crates/l/http1-syntax.svg)](https://github.com/goliajp/stones)

RFC 7230 HTTP/1.1 syntax primitives — pure Rust,
**zero external deps** (only `byte-cursor` from the same
`stones/` workspace), `no_std + alloc`.

## Spec

[RFC 7230 — Hypertext Transfer Protocol (HTTP/1.1):
Message Syntax and
Routing](https://datatracker.ietf.org/doc/html/rfc7230).

Specifically:
- § 3.2.6 — Field Value Components (token character set,
  quoted-string, comment)
- § 6.1 — Connection (token list semantics)

## Features

| Feature | Status |
|---|---|
| `is_token_char(b: u8) -> bool` (RFC 7230 § 3.2.6) | ✅ |
| `is_token(s: &[u8]) -> bool` | ✅ |
| `read_token(cursor: &mut Cursor) -> &[u8]` | ✅ |
| `read_quoted_string(cursor: &mut Cursor) -> Result<String, _>` | ✅ |
| `header_name_eq(a: &str, b: &str) -> bool` (case-insensitive ASCII) | ✅ |
| `token_list_contains(value: &str, token: &str) -> bool` (RFC 7230 § 7) | ✅ |
| `parse_token_list(value: &str) -> Vec<&str>` | ✅ |
| `unquote_value(value: &str) -> Result<String, _>` (handles quoted or bare) | ✅ |

`http1-syntax` does **NOT** include:

- Full HTTP/1.1 message parsing (start-line, headers,
  body). That's a separate, larger concern.
- HTTP/2 or HTTP/3 syntax (different grammar).
- Header de-duplication, comma-folding semantics
  (RFC 7230 § 3.2.2 list-field handling — caller's
  concern; we give you the primitives).

## Usage

```rust
use http1_syntax::{is_token_char, header_name_eq, token_list_contains, read_token};
use byte_cursor::Cursor;

// Token char set.
assert!(is_token_char(b'a'));
assert!(is_token_char(b'-'));
assert!(!is_token_char(b' '));

// Case-insensitive header name match.
assert!(header_name_eq("Upgrade", "upgrade"));
assert!(header_name_eq("Sec-WebSocket-Key", "sec-websocket-key"));

// Connection-header token-list lookup.
assert!(token_list_contains("keep-alive, Upgrade", "upgrade"));
assert!(!token_list_contains("keep-alive", "upgrade"));

// Token extraction.
let mut c = Cursor::from_str("permessage-deflate;");
let token = read_token(&mut c);
assert_eq!(token, b"permessage-deflate");
```

## Why a separate crate

The `grpcx-ws` WebSocket layer hand-rolled this twice
(`handshake.rs` for the upgrade-request headers,
`extensions.rs` for `Sec-WebSocket-Extensions` parsing).
Per the
[stone / steel / cement methodology][methodology],
"written ≥ 2 times" triggers extraction.

[methodology]: ../../../grpcx/ARCHITECTURE.md

`http1-syntax` collects the small, spec-anchored bits any
HTTP/1.1 parser needs without claiming to be a full
parser like `httparse`.

## Versioning

Once `1.0` ships, semver-strict. RFC 7230 has been stable
since 2014.

## License

Apache-2.0 OR MIT.
