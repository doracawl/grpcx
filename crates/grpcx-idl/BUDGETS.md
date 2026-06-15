# `grpcx-idl` Performance Budgets

Hot / warm / cold path budgets, enforced once backends land
(perf_gate wiring planned for v0.7+ once a baseline exists).
Project priority order is **perf > stability > mem > binsize**
(per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `rkyv` (`compare_rkyv.rs`, lands v0.7).

## v1.0 status (project v7.0)

L7 closed at v1.0 (2026-06-09). The full PERF tier (cycle counts
on the steady-state HOT path, `compare_rkyv` harness numbers)
defers to v1.x — same charter as L1-L6 perf passes. MEM +
BINSIZE land here.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | `to_bytes(small struct)` | v0.3 | Per-message encode userspace cost |
| HOT | `from_bytes::<T>(&[u8])` | v0.3 | Zero-copy archived view cast (no allocation) |
| HOT | `Archived<T>` field read | v0.3 | Per-field access cost on archived view |
| WARM | `Deserialize<D>` (build owned `T`) | v0.4 | Materialise owned value when zero-copy doesn't fit |
| WARM | derive-macro proc-macro expansion | v0.6 | Compile-time cost |
| COLD | `compare_rkyv` head-to-head | v0.7 | Apples-to-apples wire bench |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_rkyv`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public types — `Error`,
`Archived<T>` per shape. Numbers land v0.7.

Measured on aarch64-apple-darwin (16-byte align, 8-byte pointers).

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `Error` | 24 | 32 | `Custom(&'static str)` carries 16 + 8 disc |
| `ArchivedTuple2<u32, u32>` | 8 | 8 | exact |
| `ArchivedTuple3<u32, u16, u8>` | 8 | 8 | exact |
| `ArchivedTuple4<u8, u16, u32, u64>` | 16 | 16 | exact |
| `ArchivedOption<u32>` | 8 | 8 | exact |
| `ArchivedOption<u64>` | 16 | 16 | exact |
| `ArchivedResult<u32, u32>` | 8 | 8 | exact |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via
`cargo bloat --release --example min_idl --crates` against the
canonical `examples/min_idl.rs` (one encode + decode round-trip
of a moderately-sized struct; lands v0.3+).
Regression budget: ≤ 5 % growth per minor is noise; ≥ 10 %
stops the release.

| Version | Total binary | `grpcx-idl` contribution | Notes |
|---|---:|---:|---|
| v1.0 | 395 KiB | 424 bytes (0.2 % `.text`) | release profile (`lto=fat`, `panic=abort`, `strip=symbols`); aarch64-apple-darwin |

v1.0 break-down (`.text` share):

| Crate | Size | Share | Notes |
|---|---:|---:|---|
| `std` | 200.5 KiB | 98.4 % | unavoidable for `std` build |
| `[Unknown]` | 3.8 KiB | 1.8 % | linker glue |
| `min_idl` | 2.6 KiB | 1.3 % | example glue |
| **`grpcx_idl`** | **424 B** | **0.2 %** | L7 itself |
| `__rustc` | 212 B | 0.1 % | runtime bits |

L7 is the smallest non-glue contribution of any layer in the
stack (L6's `grpcx_wt` was the previous low at ~20 KiB). The
codec is mostly inlined into the user's call sites by the
monomorphisation passes; what's left in `grpcx_idl`'s own text
is the `bool` validation arm + the `Error` type. The full
`Vec<u8>` / `String` / tuple / array / Option / Result wire
codecs effectively cost zero binary bytes in a min-shaped
consumer.
