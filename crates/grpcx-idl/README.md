# grpcx-idl

[![crates.io](https://img.shields.io/crates/v/grpcx-idl.svg)](https://crates.io/crates/grpcx-idl)
[![docs.rs](https://docs.rs/grpcx-idl/badge.svg)](https://docs.rs/grpcx-idl)
[![license](https://img.shields.io/crates/l/grpcx-idl.svg)](https://github.com/goliajp/grpcx)

grpcx's own IDL + zero-copy wire format (L7). The
rkyv / Cap'n Proto cost-class layer — schema-driven binary
encoding with `from_bytes(&[u8]) -> &Archived<T>` semantics. No
runtime deps; `no_std + alloc`-compatible.

## Status (v1.0 — project v7.0)

Released 2026-06-09. Stability promise: the v1.x public
surface is semver-stable. Working bits:

- **Primitive codec** — u8/u16/u32/u64/u128, i8/i16/i32/i64/i128,
  f32/f64, bool — all LE fixed-width, `Archived<T> = T` on
  little-endian targets (`from_bytes` is a length + alignment
  check followed by transmute).
- **Composite types** — fixed-size tuples 2-8 + fixed arrays
  `[T; N]`. Wire format matches `#[repr(C)]` ABI exactly.
- **Variable-length** — `Vec<u8>` + `String` with `u32 LE`
  length prefix. `ArchivedBytes::as_slice()` + lazy
  `ArchivedString::as_str() -> Result<&str, Utf8>`.
- **Tagged unions** — `Option<T>` + `Result<T, E>` on a
  `#[repr(u32)]` discriminant layout. Per-variant payload
  validation; trailing pad explicit.
- **`#[derive(GrpcxIdl)]`** — proc-macro crate
  [`grpcx-idl-derive`](../grpcx-idl-derive) auto-generates
  Archive / Serialize / Deserialize impls for user named-field
  structs and unit + single-payload tuple-variant enums.

Out of scope for v1.0 — deferred to v1.x (see
[CHANGELOG](./CHANGELOG.md#deferred-to-v1x)):

- Typed `Vec<T>` / `&[T]` for `T ≠ u8` (needs alignment
  padding when `align_of::<T>() > 4`).
- Standalone borrowed `&[u8]` / `&str` (the `Archive` trait's
  associated type can't carry a lifetime; v1.x adds an
  `ArchiveRef` companion trait).
- Tuple structs / multi-payload tuple variants / named-field
  enum variants in `#[derive(GrpcxIdl)]`.
- Huffman / variable-width discriminant compaction.
- `compare_rkyv` head-to-head publish (deferred to v1.x perf
  pass).

## When to reach for this vs `rkyv` / `capnp`

Use `grpcx-idl` when you're already on the grpcx stack and want
a single dep tree — no `serde` / `rkyv` / `capnp` /
`prost` / `protoc` mix. The cost-class target is rkyv's
zero-copy decode + Cap'n Proto's offset-encoded layout, in a
crate that follows the grpcx charter (no libc, no async runtime
bound, `no_std + alloc`).

Use `rkyv` / `capnp` directly when you need ecosystem
compatibility (`serde` interop, an existing schema migration
workflow, etc.) — they're mature and their cost class is the
goal, not the obstacle.

## Layer position

L7 in the grpcx stack:

```text
L1  grpcx-io        ✅ v1.1
L2  grpcx-rt        ✅ v1.0
L3  grpcx-crypto    ✅ v1.0
L4  grpcx-quic      ✅ v1.2
L5  grpcx-h3        ✅ v1.2
L6  grpcx-wt        ✅ v1.0
L7  grpcx-idl       🚧 v0.1 (this crate)
L8  grpcx-pb        ☐  (.proto editions importer; atop L7)
L9  grpcx-ws        ☐
L10 grpcx           ☐
```

L7 is a different design class from L1-L6: a **pure-codec
layer**. No syscalls, no IO, no async, no network. The schema
+ wire format here is what L8's `.proto` importer translates
to, and what L10's RPC stubs eventually use. See
[../../docs/v7-scope.md](../../docs/v7-scope.md).
