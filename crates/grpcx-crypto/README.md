# grpcx-crypto

[![crates.io](https://img.shields.io/crates/v/grpcx-crypto.svg)](https://crates.io/crates/grpcx-crypto)
[![docs.rs](https://docs.rs/grpcx-crypto/badge.svg)](https://docs.rs/grpcx-crypto)
[![license](https://img.shields.io/crates/l/grpcx-crypto.svg)](https://github.com/goliajp/grpcx)

Crypto primitives + TLS 1.3 for the grpcx stack (L3). AEAD (AES-GCM,
ChaCha20-Poly1305) + ECDH (X25519, P-256, P-384) + signatures
(Ed25519, ECDSA, RSA) + hashes (SHA-2, HKDF, HMAC) + a TLS 1.3
connection wrapper. The `rustls`-class layer.

## Status (v1.0 — L3 closed, semver-stable; interim wrapper form)

L3 deliverable complete (project v3.0, released 2026-06-07). Stable
surface: AEAD (`Aead` trait + `Aes128Gcm` / `Aes256Gcm` /
`ChaCha20Poly1305`), Kx (`Kx` trait + `X25519` / `P256` / `P384`),
Sig (`Sig` trait + `Ed25519` / `EcdsaP256`; `RsaPkcs1` / `RsaPss`
deferred), Hash (`Hash` trait + `Sha256` / `Sha384` / `Sha512`),
`Hmac<H>`, `Hkdf<H>` (with `extract` / `from_prk` / `expand` /
`expand_label`), the TLS 1.3 connection wrapper (`TlsConfig` /
`ClientHandshake` / `ServerHandshake` / `TlsConnection`), and the
QUIC v1 key-derivation hooks (`quic::INITIAL_SALT_V1` /
`initial_secret` / `hp_secret` / `key_iv_hp` / `PacketKeys`).

Stabilisation evidence (v0.6): 95.90 % line coverage; fuzz harness
in place (3 targets, smoke-clean; 24h long-run deferred to v1.x);
78 tests across 9 KAT files covering RFC 4231 (HMAC), RFC 5869 (HKDF
A.1-A.3), RFC 8032 §7.1 (Ed25519), RFC 8439 §2.8.2 (ChaCha20-Poly1305),
NIST CAVP (AES-128/256-GCM empty-PT), RFC 9001 §A.1 (QUIC v1 initial
+ packet keys). MEM/BINSIZE: grpcx-crypto's own .text contribution
is **11.9 KiB (1.4 % of binary)** — rustls + ring + std dominate, by
design.

Stability promise: v1.x patch / minor releases preserve the public
surface above. Breaking changes require a `grpcx-crypto` v2.0. The
`#[doc(hidden)]` `__fuzz_helpers` module is explicitly **not** part
of the stable surface — same caveat as `grpcx-rt`'s.

## Two-form deliverable

The L3 series ships **two forms** of this crate:

| Form | When | This v3.x series |
|---|---|---|
| **Interim wrapper** — backed by `rustls` + `*ring*` | now → first external-cryptographer-review window | **Yes — v3.x is this.** |
| **Final self-implementation** — constant-time, hand-rolled | post external-review window | Future — re-opens once external review is reachable. |

The wrapper's API is designed **as if** the self-impl were already
here. When the swap happens, callers (L4+) need zero changes. See
[`DEPS_AUDIT.md`](../../DEPS_AUDIT.md) for the time-bounded
zero-external-dep exception that scopes `rustls` here.

## When to reach for this vs `rustls`

Same job as `rustls` — TLS 1.3 endpoints, AEAD + ECDH + signature
primitives — inside a zero-dependency-aspiring stack where every
layer is in-house and the priority order is **performance > stability
> memory > binary size**. Not drop-in `rustls`-compatible; a
deliberately-narrower API surface that the future self-impl will keep.

## Design philosophy

Part of the larger [grpcx](../../README.md) project. Read in order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md) — stones / cement / layer
   discipline / priority axes
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md) — why every layer is rewritten
   (and the L3 wrapper exception)
3. [ROADMAP.md](../../ROADMAP.md) — L1 → L10 with per-layer triggers
4. [docs/v3-scope.md](../../docs/v3-scope.md) — L3 scope + 11 work
   units + checkpoints + per-checkpoint design notes

## License

Apache-2.0 OR MIT
