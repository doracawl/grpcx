# grpcx-quic

[![crates.io](https://img.shields.io/crates/v/grpcx-quic.svg)](https://crates.io/crates/grpcx-quic)
[![docs.rs](https://docs.rs/grpcx-quic/badge.svg)](https://docs.rs/grpcx-quic)
[![license](https://img.shields.io/crates/l/grpcx-quic.svg)](https://github.com/goliajp/grpcx)

QUIC v1 transport for the grpcx stack (L4). RFC 9000 (transport)
+ RFC 9001 (TLS integration) + RFC 9002 (loss recovery), over
[`grpcx-io`](../grpcx-io)'s UDP socket primitives,
[`grpcx-rt`](../grpcx-rt)'s async runtime,
[`grpcx-crypto`](../grpcx-crypto)'s TLS 1.3 connection wrapper +
QUIC key-derivation hooks. The `quinn`-class layer.

## Status (v1.0 — project v4.0)

Released 2026-06-08. Stability promise: the v1.x public surface
is semver-stable. Working bits:

- **Wire-format primitives** — varint codec (RFC 9000 §16),
  packet header codec for Long / Short forms (RFC 9000 §17),
  frame codec for all 21 RFC 9000 §19 + RFC 9221 frames, AEAD
  payload protection + header protection across AES-128-GCM /
  AES-256-GCM / ChaCha20-Poly1305 (RFC 9001 §5.3 / §5.4).
- **Initial-packet helper** — derives Initial keys from the
  client's original DCID, builds + parses Initial packets with
  CRYPTO frames and PADDING-to-1200 (RFC 9000 §14.1).
- **Connection state machine** (`QuicConnection`) — drives a
  full handshake to 1-RTT via `grpcx_crypto::quic_handshake`'s
  `rustls::quic` wrapper, multiplexes bidirectional streams,
  exchanges application data both directions. v0.6 loopback test
  is the canonical acceptance.

Out of scope for v1.0 — deferred to v1.x (see
[CHANGELOG](./CHANGELOG.md#deferred-to-v1x)):

- Loss recovery + congestion control (RFC 9002).
- Real UDP socket binding (v1.0 runs over an in-memory bus; the
  `pump`-shaped API connects directly to `grpcx-io` in v1.x).
- 0-RTT, connection migration, coalesced packets, Retry, Version
  Negotiation.
- Self-impl crypto provider (v1.0 uses the L3 interim wrapper
  around `rustls::quic`; the all-in-house path retires the
  `rustls` + `ring` deps post-external-cryptographer-review).

## When to reach for this vs `quinn`

Same job as `quinn` — QUIC endpoints, connections, streams —
inside a zero-dependency stack where every layer is in-house and
the priority order is **performance > stability > memory > binary
size**. Not drop-in `quinn`-compatible; a deliberately-narrower
API surface.

## Design philosophy

Part of the larger [grpcx](../../README.md) project. Read in
order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md) — stones / cement /
   layer discipline / priority axes
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md) — why every layer is
   rewritten
3. [ROADMAP.md](../../ROADMAP.md) — L1 → L10 with per-layer
   triggers
4. [docs/v4-scope.md](../../docs/v4-scope.md) — L4 scope + 11
   work units + checkpoints + per-checkpoint design notes

## License

Apache-2.0 OR MIT
