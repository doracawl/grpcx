# grpcx-pb

[![crates.io](https://img.shields.io/crates/v/grpcx-pb.svg)](https://crates.io/crates/grpcx-pb)
[![docs.rs](https://docs.rs/grpcx-pb/badge.svg)](https://docs.rs/grpcx-pb)
[![license](https://img.shields.io/crates/l/grpcx-pb.svg)](https://github.com/goliajp/grpcx)

`.proto` (editions 2023+) importer for the grpcx stack (L8).
Translates `.proto` schemas into [L7
IDL](../grpcx-idl) definitions, then emits Rust source code
carrying `#[derive(GrpcxIdl)]`. Optional runtime wire-format
shim lets legacy callers speak `.proto`-on-wire while internals
stay L7. No runtime deps; `no_std + alloc`-compatible.

## Status (v0.1 — API skeleton)

API skeleton + design docs synced 2026-06-10. The public surface
is frozen with `unimplemented!()` bodies — `import(&str)`,
`Schema` / `Message` / `Field` / `Enum` types, `ImportError`
enum. Implementation lands across v0.2-v0.7; see
[`../../REFACTOR-v8-v0.1-bootstrap.md`](../../REFACTOR-v8-v0.1-bootstrap.md).

## Scope (v8 final form, see [`docs/v8-scope.md`](../../docs/v8-scope.md))

- **Editions-2023-shaped kernel.** The in-memory model is the
  editions grammar; `syntax = "proto3"` (1.3, DX stage A) and
  `syntax = "proto2"` (1.5, DX stage C) sources are translated
  onto it at the frontend. Unknown editions are rejected at the
  syntax-line lexer state with `UnsupportedFeature`.
- **Single-file, no implicit IO.** `import(&str)` takes raw
  `.proto` source; multi-file resolution is the caller's
  problem.
- **Codegen is text.** L8 emits Rust source as a `String`;
  caller decides whether to feed it to a `build.rs`, a proc-
  macro, or a one-shot codegen binary.
- **Runtime shim.** A `.proto` binary ↔ L7 IDL adapter lets
  legacy callers stay on `.proto`-on-wire.

## When to reach for this vs `prost` / `protoc`

Use `grpcx-pb` when you're already on the grpcx stack and want
a single dep tree — no `prost` / `protoc` / `protobuf` mix. The
codegen output is L7 IDL (zero-copy decode) rather than tagged-
field wire (prost's shape).

Use `prost` / `protoc` directly when you need the existing
ecosystem (`tonic`, the protobuf compiler plugins, the Google
conformance suite as the source of truth) — they're mature and
their compatibility surface is the goal, not the obstacle.

## Layer position

L8 in the grpcx stack:

```text
L1  grpcx-io        ✅ v1.1
L2  grpcx-rt        ✅ v1.0
L3  grpcx-crypto    ✅ v1.2
L4  grpcx-quic      ✅ v1.2
L5  grpcx-h3        ✅ v1.2
L6  grpcx-wt        ✅ v1.0
L7  grpcx-idl       ✅ v1.0
L8  grpcx-pb        🚧 v0.1 (this crate)
L9  grpcx-ws        ☐
L10 grpcx           ☐
```

L8 is a **language layer** — lexer / parser / semantic analyser
over `.proto` source, plus a codegen target that emits L7 IDL
Rust source. No syscalls, no IO, no async, no network. See
[`../../docs/v8-scope.md`](../../docs/v8-scope.md).
