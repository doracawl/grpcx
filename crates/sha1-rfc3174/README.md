# sha1-rfc3174

[![crates.io](https://img.shields.io/crates/v/sha1-rfc3174.svg)](https://crates.io/crates/sha1-rfc3174)
[![docs.rs](https://docs.rs/sha1-rfc3174/badge.svg)](https://docs.rs/sha1-rfc3174)
[![license](https://img.shields.io/crates/l/sha1-rfc3174.svg)](https://github.com/goliajp/stones)

RFC 3174 SHA-1 — pure Rust, **zero deps**, `no_std +
alloc`, spec-anchored.

## ⚠️ Security warning

SHA-1 is **cryptographically broken** since 2017 (the
SHAttered collision). **Do NOT use this crate for**:

- Password hashing
- Digital signatures
- TLS / SSH key fingerprints (modern variants moved off
  SHA-1)
- Any new security-sensitive design

**Legitimate uses that survive the deprecation**:

- RFC 6455 WebSocket handshake (`Sec-WebSocket-Accept`
  derivation — uses SHA-1 by spec, no security
  implication; the value is a magic-cookie echo)
- Git object hashing (compat with legacy repos)
- HMAC-SHA1 in legacy protocol interop
- Replication of historical test vectors

If you're not sure your use is one of the above, reach
for SHA-256 or SHA-3 instead.

## Spec

[RFC 3174 — US Secure Hash Algorithm 1
(SHA1)](https://datatracker.ietf.org/doc/html/rfc3174).

Specifically:
- § 5 — Functions and Constants Used
- § 6 — Computing the Message Digest
- § 7 — SHA-1 Sample Outputs (full test-vector lock)

## Features

| Feature | Status |
|---|---|
| One-shot `sha1(data: &[u8]) -> [u8; 20]` | ✅ |
| Incremental `Sha1 { update, finalize }` | ✅ |
| Hex-output helper `sha1_hex(data) -> String` | ✅ |
| RFC 3174 § 7 test vectors (incl. 1M-`a` long input) | ✅ |
| `no_std + alloc` | ✅ |
| `no_std` without `alloc` (one-shot only) | ✅ |
| Constant-time | ❌ (not meaningful for a hash — output is public) |

## Usage

```rust
use sha1_rfc3174::{sha1, sha1_hex, Sha1};

// One-shot
let digest = sha1(b"abc");
assert_eq!(
    digest,
    [
        0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a,
        0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2, 0x6c,
        0x9c, 0xd0, 0xd8, 0x9d,
    ]
);

// Hex helper
assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");

// Incremental (streaming) — same digest as one-shot.
let mut hasher = Sha1::new();
hasher.update(b"ab");
hasher.update(b"c");
assert_eq!(hasher.finalize(), sha1(b"abc"));
```

## Why another sha1 crate?

- **Zero deps** — the popular `sha1` crate at v0.10
  depends on `cfg-if`, `cpufeatures`, `digest`,
  `generic-array`, `typenum`, etc. for SIMD detection
  and the `digest::Digest` trait. `sha1-rfc3174` doesn't.
- **Fast** — on AArch64 with the SHA crypto extensions
  (Apple Silicon, ARMv8-A servers) a software-pipelined
  hardware path (`SHA1C`/`SHA1P`/`SHA1M` + scheduled
  next-block loads) measures **ahead of OpenSSL's and Go's
  SHA-1 at 1 MiB** and 2.3× the community `sha1` crate's
  default build (see BUDGETS.md for the full table and
  methodology). Portable scalar fallback everywhere else.
- **Contained `unsafe`** — `unsafe` appears ONLY inside
  the `aarch64_sha` hardware module, confined to raw
  16-byte vector loads/stores with slice-bounds SAFETY
  arguments. The scalar reference implementation is 100%
  safe Rust, stays in-tree unconditionally, and the test
  suite differential-tests the hardware path against it
  at every input length 0..=600.
- **Spec-anchored** — RFC section numbers cited inline.
- **Lifted from a real codebase** —
  [grpcx-ws][grpcx-ws] needs SHA-1 for the RFC 6455
  handshake; the inline impl is the seed for this crate.

[grpcx-ws]: ../../../grpcx/crates/grpcx-ws

## Versioning

Once `1.0` ships, this crate follows semver strictly. The
RFC 3174 spec hasn't changed since 2001.

## License

Apache-2.0 OR MIT.
