# `grpcx-pb` Performance Budgets

Hot / warm / cold path budgets, enforced once backends land
(perf_gate wiring planned for v0.7+ once a baseline exists).
Project priority order is **perf > stability > mem > binsize**
(per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `prost` (`compare_prost.rs`, lands
v0.7).

## v1.0 status (project v8.0)

L8 closed at v1.0 (2026-06-10). MEM + BINSIZE filled below.
Full PERF tier (cycle counts on the steady-state HOT path,
`compare_prost` head-to-head numbers) defers to v8.1 / v1.x
— same charter as L1-L7.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| WARM | `import(source: &str)` end-to-end | v0.4 | One-shot lex + parse + semantic over a small `.proto` |
| WARM | `codegen(&Schema) -> String` | v0.5 | Rust source emission cost per schema |
| HOT | shim `.proto` binary → L7 IDL decode | v0.6 | Per-message wire decode through the runtime shim |
| HOT | shim L7 IDL → `.proto` binary encode | v0.6 | Per-message wire encode through the runtime shim |
| COLD | `compare_prost` head-to-head | v0.7 | Apples-to-apples codegen + wire bench |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_prost`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public ADTs. Measured on
aarch64-apple-darwin (16-byte align, 8-byte pointers) via
`crates/grpcx-pb/src/sizes.rs::report_internal_sizes`. Each
row is also checked at compile time by a `const _: () = assert!(…)`
in `sizes.rs`; the build fails if any size exceeds its
bound.

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `ImportError` | 24 | 32 | enum: `(u32, u32)` for Lex/Parse + `&'static str` for Semantic/UnsupportedFeature |
| `WireError` | 24 | 32 | enum: `Truncated` / `InvalidVarint` / `InvalidTag` + `u32` for UnknownWireType + `&'static str` for UnsupportedFeature |
| `WireType` | 1 | 4 | repr-default enum, 6 variants |
| `Edition` | 0 | 4 | single-variant enum |
| `Label` | 1 | 4 | two-variant enum |
| `FieldType` | 32 | 48 | recursive via `Box<FieldType>` for `Map { key, value }` |
| `Field` | 64 | 64 | `(String, u32, FieldType, Label)` plus alignment |
| `EnumValue` | 32 | 32 | `(String, i32)` |
| `Enum` | 48 | 64 | `(String, Vec<EnumValue>)` |
| `Message` | 96 | 96 | `(String, Vec<Field>, Vec<Message>, Vec<Enum>)` |
| `Schema` | 96 | 128 | `(Edition, Option<String>, Vec<String>, Vec<Message>, Vec<Enum>)` |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via
`cargo bloat --release --example min_pb --crates` against
`examples/min_pb.rs` (one `import` + `codegen` + ProtoCodec
round-trip on `Greeting { name: string, age: int32 }`).
Regression budget: ≤ 5 % growth per minor is noise; ≥ 10 %
stops the release.

| Version | Total binary `.text` | `grpcx-pb` contribution | Notes |
|---|---:|---:|---|
| v1.0 | 250.6 KiB | 27.1 KiB (10.8 % `.text`) | release profile (`lto=fat`, `panic=abort`, `strip=symbols`); aarch64-apple-darwin |

v1.0 break-down (`.text` share):

| Crate | Size | Share | Notes |
|---|---:|---:|---|
| `std` | 215.0 KiB | 85.8 % | unavoidable for `std` build |
| **`grpcx_pb`** | **27.1 KiB** | **10.8 %** | L8 itself: lex + parse + semantic + codegen + wire primitives |
| `min_pb` | 10.5 KiB | 4.2 % | example glue (hand-written `Greeting` + `ProtoCodec` impl + assertions) |
| `[Unknown]` | 3.8 KiB | 1.5 % | linker glue |
| `__rustc` | 212 B | 0.1 % | runtime bits |

L8 is significantly larger than L7 (which was 424 B / 0.2 %)
— expected, since L8 ships a full lexer + parser + semantic
analyser + codegen + wire shim, whereas L7 was a pure codec
trait. The 27 KiB figure is dominated by the parser's
recursive-descent state plus codegen's emit logic; both
inline aggressively in release mode, so a consumer that only
calls the wire primitives can expect a substantially smaller
contribution.
