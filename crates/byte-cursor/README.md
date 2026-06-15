# byte-cursor

[![crates.io](https://img.shields.io/crates/v/byte-cursor.svg)](https://crates.io/crates/byte-cursor)
[![docs.rs](https://docs.rs/byte-cursor/badge.svg)](https://docs.rs/byte-cursor)
[![license](https://img.shields.io/crates/l/byte-cursor.svg)](https://github.com/goliajp/stones)

Position-tracking cursor over a byte slice. Pure Rust,
**zero deps**, `no_std` with no `alloc` required.

## What it is

A `Cursor` over `&[u8]` with:

- `peek` / `peek_at(ofs)` / `advance` — byte-level
  inspection + consumption.
- `pos` / `remaining` / `at_end` — position queries.
- `line` / `col` / `here` — 1-based line/column
  tracking (incremented on `b'\n'`).
- `take_while(pred)` / `skip_while(pred)` —
  predicate-driven span extraction + skipping.
- `skip_ws_horizontal` / `skip_ws` — common whitespace
  shorthands.

That's it. The crate intentionally does not include
character classification (RFC 7230 token / `.proto` ident
/ etc.) — those live in their own spec-anchored stones
or in your application code.

## Why a separate crate

Two `grpcx-*` parsers (the `.proto` lexer and the HTTP/1
extension-header parser) each hand-rolled the same small
byte-cursor abstraction. Per the
[stone / steel / cement methodology][methodology], "code
written ≥ 2 times" is the standard trigger for extraction.

[methodology]: ../../../grpcx/ARCHITECTURE.md

The lifted cursor unifies their shape:

- The `.proto` lexer's version tracked line/col (for
  error reporting).
- The HTTP/1 parser's version had `skip_ws` (for
  RFC 7230 LWS handling).

`byte-cursor` carries both, opt-in.

## Usage

```rust
use byte_cursor::Cursor;

let mut c = Cursor::from_str("hello world");
assert_eq!(c.peek(), Some(b'h'));

// take_while for `[a-z]+`
let word = c.take_while(|b| b.is_ascii_lowercase());
assert_eq!(word, b"hello");

// Skip the space.
assert_eq!(c.advance(), Some(b' '));

// position + line/col tracking.
assert_eq!(c.pos(), 6);
assert_eq!(c.here(), (1, 7));

// Read the rest.
let word = c.take_while(|b| b.is_ascii_lowercase());
assert_eq!(word, b"world");
assert!(c.at_end());
```

## Features

| Feature | Status |
|---|---|
| `peek` / `peek_at(ofs)` / `advance` | ✅ |
| `pos` / `remaining` / `at_end` | ✅ |
| `line` / `col` / `here` (1-based, `\n` aware) | ✅ |
| `take_while(pred)` / `skip_while(pred)` | ✅ |
| `skip_ws_horizontal` (` \t`) | ✅ |
| `skip_ws` (` \t\r\n`) | ✅ |
| `expect_byte(b)` / `try_consume(b)` | ✅ |
| UTF-8 / `char`-level iteration | ❌ (this is a `byte` cursor — wrap with `str::from_utf8` if you need chars) |
| Backtracking via `mark` / `reset` | ❌ (v1.x if requested — current users haven't needed it) |

## Versioning

Once `1.0` ships, semver-strict.

## License

Apache-2.0 OR MIT.
