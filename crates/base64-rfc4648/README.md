# base64-rfc4648

[![crates.io](https://img.shields.io/crates/v/base64-rfc4648.svg)](https://crates.io/crates/base64-rfc4648)
[![docs.rs](https://docs.rs/base64-rfc4648/badge.svg)](https://docs.rs/base64-rfc4648)
[![license](https://img.shields.io/crates/l/base64-rfc4648.svg)](https://github.com/goliajp/stones)

RFC 4648 base64 — pure Rust, **zero deps**, `no_std +
alloc`, spec-anchored.

## Spec

[RFC 4648 — The Base16, Base32, and Base64 Data
Encodings](https://datatracker.ietf.org/doc/html/rfc4648).

Specifically:
- § 4 — Base 64 Encoding (Standard alphabet)
- § 5 — Base 64 Encoding with URL and Filename Safe
  Alphabet
- § 3.2 — Padding of Encoded Data (mandatory `=`)
- § 10 — Test Vectors (canonical test set, all locked
  in our KAT)

## Features

| Feature | Status |
|---|---|
| Standard alphabet (`A-Z a-z 0-9 + /`) | ✅ |
| URL-safe alphabet (`A-Z a-z 0-9 - _`) | ✅ |
| Encoding with `=` padding | ✅ |
| Decoding (padding-strict, the spec default) | ✅ |
| Decoding (padding-tolerant, accepts missing `=`) | ✅ |
| Invalid-character rejection | ✅ |
| Whitespace rejection in `Strict` mode | ✅ |
| Reject "overlong" decoded length (non-canonical last char) | ✅ |
| `no_std + alloc` | ✅ |
| Constant-time semantics | ❌ (not needed for base64; v1.x adds opt-in `ct` feature if a host demands) |
| Streaming API (chunk by chunk) | ❌ (v1.x — current API is one-shot `&[u8]` ↔ `String`/`Vec<u8>`) |

## Usage

```rust
use base64_rfc4648::{encode, decode, encode_with, decode_with, Alphabet};

let bytes = b"hello world";
let encoded = encode(bytes);
assert_eq!(encoded, "aGVsbG8gd29ybGQ=");

let decoded = decode(&encoded).unwrap();
assert_eq!(decoded, bytes);

// URL-safe variant
let encoded_urlsafe = encode_with(bytes, Alphabet::UrlSafe);
let decoded_urlsafe = decode_with(&encoded_urlsafe, Alphabet::UrlSafe).unwrap();
assert_eq!(decoded_urlsafe, bytes);
```

## Why another base64 crate?

- **Zero deps** — adding `base64` from crates.io pulls
  no transitive deps today, but its build chain (proc-
  macros, build scripts) is heavier than needed for what
  amounts to a few `<<` and `>>` ops.
- **Fast** — on AArch64, NEON lanes (aklomp reshuffle
  encode, Muła nibble-LUT decode) measure **18-24 GiB/s
  encode / 11-13 GiB/s decode on Apple M4 — 3-6× the
  community `base64` crate, OpenSSL, and Go's stdlib at
  every benchmarked size** (BUDGETS.md has the full table
  + methodology). Elsewhere, tuned scalar paths (u64
  encode lane, pre-shifted u32 decode tables) still lead
  the same comparator set.
- **Contained `unsafe`** — confined to the NEON
  vector-load/store module and the raw output writes into
  reserved capacity, each with an explicit bounds SAFETY
  argument. All parsing/validation logic is safe Rust;
  the NEON validity masks are differentially verified
  against the scalar decode table for all 256 byte
  values; the RFC 4648 § 3.5 canonical-form +
  error-priority contracts are KAT-locked.
- **Spec-anchored README** — the RFC section numbers are
  cited inline so you can audit conformance directly.
- **Lifted from a real codebase** — `base64-rfc4648` was
  extracted from `grpcx-ws`'s WebSocket handshake
  implementation, where the RFC 6455 § 1.3 reference
  vector is byte-locked. The extraction preserves that
  KAT.

## Versioning

Once `1.0` ships, this crate follows semver strictly. The
RFC 4648 spec is stable since 2006; any breaking change
to the public API requires a major bump.

## License

Apache-2.0 OR MIT.
