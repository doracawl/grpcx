# grpcx-h3

[![crates.io](https://img.shields.io/crates/v/grpcx-h3.svg)](https://crates.io/crates/grpcx-h3)
[![docs.rs](https://docs.rs/grpcx-h3/badge.svg)](https://docs.rs/grpcx-h3)
[![license](https://img.shields.io/crates/l/grpcx-h3.svg)](https://github.com/goliajp/grpcx)

HTTP/3 for the grpcx stack (L5). RFC 9114 (HTTP/3 over QUIC) +
RFC 9204 (QPACK field compression), atop
[`grpcx-quic`](../grpcx-quic)'s `QuicConnection` surface,
[`grpcx-crypto`](../grpcx-crypto)'s TLS 1.3 / QUIC handshake
wrapper, [`grpcx-rt`](../grpcx-rt)'s async runtime, and
[`grpcx-io`](../grpcx-io)'s UDP socket primitives. The `h3`-class
layer.

## Status (v1.0 — project v5.0)

Released 2026-06-09. Stability promise: the v1.x public surface
is semver-stable. Working bits:

- **HTTP/3 frame codec** — every RFC 9114 §7.2 wire frame (DATA
  / HEADERS / SETTINGS / GOAWAY / MAX_PUSH_ID / CANCEL_PUSH /
  PUSH_PROMISE / ORIGIN / PRIORITY_UPDATE) encodes + decodes
  round-trip on a caller-owned buffer.
- **QPACK static table** — the full 99-entry RFC 9204 Appendix A
  static table + prefixed-integer codec + the three core
  field-line patterns (Indexed / Literal-w-NameRef / Literal-w-
  LiteralName) for the encoder.
- **QPACK dynamic table** — receiver side complete: instruction
  codecs (4 encoder + 3 decoder), per-direction `DynamicTable`
  with capacity + eviction, full RFC 9204 §4.5 field-section
  decode including Post-Base patterns, Insert-Count-Increment
  + Section-Ack feedback on the local decoder stream.
- **Stream type discriminator** — Control / Push / QpackEncoder
  / QpackDecoder / Reserved (RFC 9114 §6.2).
- **Connection state machine** (`Connection`) — drives QUIC
  handshake + opens H/3 control + QPACK uni-streams once 1-RTT
  is reached, exchanges SETTINGS, ships bidirectional request
  streams with HEADERS + DATA + FIN + GOAWAY. The v0.5 loopback
  test is the canonical acceptance.

Out of scope for v1.0 — deferred to v1.x (see
[CHANGELOG](./CHANGELOG.md#deferred-to-v1x)):

- Encoder-side dynamic-table strategy (when to insert vs
  literal-emit; RIC + Base selection). v1.0 ships encoder
  static-only; the receiver supports full dynamic refs.
- Huffman encode (decoder still rejects `H=1`).
- Trailers / server push acceptance / blocked-streams.
- `compare_h3` head-to-head publish (deferred to v1.x perf pass,
  same charter as L1-L4).
- Self-impl crypto (still uses L3's `rustls::quic` wrapper).

## When to reach for this vs `h3`

Same job as the `h3` crate — HTTP/3 framing + QPACK on top of
QUIC streams — inside a zero-dependency stack where every layer
is in-house and the priority order is **performance > stability
> memory > binary size**. Not drop-in `h3`-compatible; a
deliberately-narrower API surface.

## Design philosophy

Part of the larger [grpcx](../../README.md) project. Read in
order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md) — stones / cement /
   layer discipline / priority axes
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md) — why every layer is
   rewritten
3. [ROADMAP.md](../../ROADMAP.md) — L1 → L10 with per-layer
   triggers
4. [docs/v5-scope.md](../../docs/v5-scope.md) — L5 scope + 7
   work units + checkpoints + per-checkpoint design notes

## License

Apache-2.0 OR MIT
