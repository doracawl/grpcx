# grpcx

[![crates.io](https://img.shields.io/crates/v/grpcx.svg)](https://crates.io/crates/grpcx)
[![docs.rs](https://docs.rs/grpcx/badge.svg)](https://docs.rs/grpcx)
[![license](https://img.shields.io/crates/l/grpcx.svg)](https://github.com/goliajp/grpcx)

gRPC core for the grpcx stack (L10). Service trait +
client/server runtime over HTTP/3 (`grpcx/h3`) and
WebSocket (`grpcx/ws-mux`) transports. The
**top-of-stack** layer that knits L4-L9 together; no
`tonic`, no `prost`, no `hyper`.

## Status (v0.1 — API skeleton)

API skeleton + design docs synced 2026-06-10. The public
surface is frozen with `unimplemented!()` bodies —
`Service` trait + `Method` / `RpcKind` / `Status` /
`StatusCode` / `Metadata` / `RpcError` ADTs + `Channel`
+ `Server` stubs + ALPN constants. Implementation lands
across v0.2-v0.7; see
[`../../REFACTOR-v10-v0.1-bootstrap.md`](../../REFACTOR-v10-v0.1-bootstrap.md).

## Scope (v10 final form, see [`docs/v10-scope.md`](../../docs/v10-scope.md))

- **Service abstraction** — `Service` trait + `Method`
  metadata + `RpcKind` (unary / server-stream / client-
  stream / bidi).
- **Status surface** — `Status` + `StatusCode` (gRPC's
  17 standard codes 0..=16) + `Metadata` (headers).
- **Transport negotiation** — ALPN-based on the wire
  (`grpcx/h3` preferred, `grpcx/ws-mux` fallback). HTTP/2
  is not in the fallback chain — it is the explicit
  **interop bridge** (charter revision 2026-06-11): h2c
  via `ConnectedChannel::connect_h2c` /
  `Server::listen_h2c`, live-interop-verified against
  tonic in both directions.
- **Browser path** — WebTransport first (L6); WebSocket+
  ws-mux fallback. Servers advertise WT via `Alt-Svc`.
- **Wire-compat** — same `.proto` → wire-equivalent with
  `tonic` (per the trigger).

## When to reach for this vs `tonic` / `grpcio`

Use `grpcx` when you're already on the grpcx stack and
want a single dep tree — no `tonic` / `prost` / `hyper`
mix. The transport, codec, and IDL all come from
grpcx-* layers; the dep graph is `grpcx`-only.

Use `tonic` directly when you need the existing gRPC
ecosystem (interop with services using `prost`-generated
types, server reflection, `tower` middleware, etc.) —
it's mature and our charter overlap with it is by
design.

## Layer position

L10 — the **top of the stack**:

```text
L1  grpcx-io        ✅ v1.1
L2  grpcx-rt        ✅ v1.0
L3  grpcx-crypto    ✅ v1.2
L4  grpcx-quic      ✅ v1.2
L5  grpcx-h3        ✅ v1.2
L6  grpcx-wt        ✅ v1.0
L7  grpcx-idl       ✅ v1.0
L8  grpcx-pb        ✅ v1.0
L9  grpcx-ws        ✅ v1.0
L10 grpcx           🚧 v0.1 (this crate — final layer)
```

L10 is an **orchestration layer**: it depends on L4
(QUIC), L5 (HTTP/3), L6 (WebTransport), L8 (`.proto`
codegen), and L9 (WebSocket) but doesn't reimplement
their primitives. See
[`../../docs/v10-scope.md`](../../docs/v10-scope.md).
