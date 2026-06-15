# grpcx-wt

[![crates.io](https://img.shields.io/crates/v/grpcx-wt.svg)](https://crates.io/crates/grpcx-wt)
[![docs.rs](https://docs.rs/grpcx-wt/badge.svg)](https://docs.rs/grpcx-wt)
[![license](https://img.shields.io/crates/l/grpcx-wt.svg)](https://github.com/goliajp/grpcx)

WebTransport for the grpcx stack (L6). RFC 9220 (Bootstrapping
WebTransport with HTTP/3) + draft-ietf-webtrans-http3, atop
[`grpcx-h3`](../grpcx-h3)'s `Connection` surface. The
`wtransport`-class layer.

## Status (v0.1 — bootstrap)

API skeleton only — the public surface is frozen and each item
carries a doc comment, but bodies are `unimplemented!()`.
Implementation lands across v0.2-v0.7:

- v0.2 — WT stream type framing + RFC 9297 H/3 datagram framing (session-id prefix)
- v0.3 — Extended CONNECT bootstrap (client emit + server accept)
- v0.4 — Session state machine (id mapping, stream + datagram dispatch)
- v0.5 — Loopback session round-trip (bidi + uni + datagram)
- v0.6 — DRAIN + CLOSE_WEBTRANSPORT_SESSION + multi-session-per-connection
- v0.7 — Stabilisation (fuzz, KAT, cov ≥ 95 %, MEM/BINSIZE, `compare_wt` harness) → project v6.0

## When to reach for this vs `wtransport`

Same job as the `wtransport` crate — WebTransport sessions over
HTTP/3 — inside a zero-dependency stack where every layer is
in-house and the priority order is **performance > stability >
memory > binary size**. Not drop-in `wtransport`-compatible; a
deliberately-narrower API surface.

## Design philosophy

Part of the larger [grpcx](../../README.md) project. Read in
order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md)
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md)
3. [ROADMAP.md](../../ROADMAP.md)
4. [docs/v6-scope.md](../../docs/v6-scope.md)

## License

Apache-2.0 OR MIT
