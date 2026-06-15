# grpcx-ws

[![crates.io](https://img.shields.io/crates/v/grpcx-ws.svg)](https://crates.io/crates/grpcx-ws)
[![docs.rs](https://docs.rs/grpcx-ws/badge.svg)](https://docs.rs/grpcx-ws)
[![license](https://img.shields.io/crates/l/grpcx-ws.svg)](https://github.com/goliajp/grpcx)

RFC 6455 WebSocket framing + HTTP/1.1 upgrade handshake
for the grpcx stack (L9). Pure framing layer — no async,
no IO, no syscalls; caller drives the byte stream. No
runtime deps; `no_std + alloc`-compatible.

## Status (v0.1 — API skeleton)

API skeleton + design docs synced 2026-06-10. The public
surface is frozen with `unimplemented!()` bodies — `Frame`,
`Opcode`, `CloseCode`, `Message`/`MessageKind`,
`Handshake`, `WsError`, plus the entry-point stubs
(`encode_frame`, `decode_frame`,
`perform_client_handshake_request`,
`parse_server_handshake_response`,
`parse_client_handshake_request`,
`build_server_handshake_response`). Implementation lands
across v0.2-v0.7; see
[`../../REFACTOR-v9-v0.1-bootstrap.md`](../../REFACTOR-v9-v0.1-bootstrap.md).

## Scope (v9 final form, see [`docs/v9-scope.md`](../../docs/v9-scope.md))

- **Frame codec** — RFC 6455 § 5.2 byte layout: header +
  opcode + masking + 7/16/64-bit length encoding.
- **Opcodes** — Continuation / Text / Binary / Close /
  Ping / Pong. Reserved opcodes rejected.
- **Close codes** — every RFC 6455 § 7.4.1 standard code
  named, plus a `Custom(u16)` arm for IANA-registered
  and application-private ranges.
- **Message reassembly** — fragmented Text/Binary
  reassemble into a single `Message`; control frame
  fragmentation rejected.
- **HTTP/1.1 upgrade handshake** — client request build +
  server response build + parse for both sides. Byte-
  wire-compatible with the RFC 6455 § 1.3 reference
  vector.
- **Extension negotiation** — surface only;
  `permessage-deflate` placeholder. Real compression
  defers to v1.x.

## When to reach for this vs `tungstenite` / `tokio-tungstenite`

Use `grpcx-ws` when you're already on the grpcx stack
and want a single dep tree — no `tungstenite` /
`tokio-tungstenite` / `httparse` mix. The framing surface
matches the RFC and integrates naturally with L1's IO
abstraction and L2's runtime.

Use `tungstenite` / `tokio-tungstenite` directly when you
need ecosystem compatibility (existing Autobahn
integration, mature `permessage-deflate` support, etc.) —
they're mature and their charter overlap with ours is by
design.

## Layer position

L9 in the grpcx stack:

```text
L1  grpcx-io        ✅ v1.1
L2  grpcx-rt        ✅ v1.0
L3  grpcx-crypto    ✅ v1.2
L4  grpcx-quic      ✅ v1.2
L5  grpcx-h3        ✅ v1.2
L6  grpcx-wt        ✅ v1.0
L7  grpcx-idl       ✅ v1.0
L8  grpcx-pb        ✅ v1.0
L9  grpcx-ws        🚧 v0.1 (this crate)
L10 grpcx           ☐
```

L9 is a **protocol layer**: pure parsing + serialisation
over a byte stream. No syscalls, no IO, no async. The
frame format here is what L10's gRPC core uses when a
client opts into WebSocket as a transport.
