# `grpcx-ws` Performance Budgets

Hot / warm / cold path budgets, enforced once backends
land (perf_gate wiring planned for v0.7+ once a baseline
exists). Project priority order is **perf > stability >
mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `tungstenite`
(`compare_tungstenite.rs`, lands v0.7+).

## v1.0 status (project v9.0)

L9 closed at v1.0 (2026-06-10). MEM + BINSIZE filled
below. Full PERF tier (cycle counts on the steady-state
HOT path, `compare_tungstenite` head-to-head numbers)
defers to v9.1 / v1.x — same charter as L1-L8.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | `decode_frame(&[u8])` | v0.2 | Per-frame decode userspace cost |
| HOT | `encode_frame(&Frame, &mut Vec<u8>)` | v0.2 | Per-frame encode cost |
| HOT | mask XOR (4-byte stride) | v0.2 | Per-byte payload masking cost |
| WARM | `Message` reassembly across fragments | v0.4 | Per-message cost |
| WARM | handshake parse / build | v0.5 | One-shot per connection |
| COLD | `compare_tungstenite` head-to-head | v0.7 | Apples-to-apples wire bench |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_tungstenite`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public ADTs. Measured on
aarch64-apple-darwin (16-byte align, 8-byte pointers) via
`crates/grpcx-ws/src/sizes.rs::report_internal_sizes`. Each
row is also checked at compile time by a
`const _: () = assert!(…)` in `sizes.rs`; the build fails
if any size exceeds its bound.

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `Opcode` | 1 | 4 | repr-default enum, 6 variants |
| `MessageKind` | 1 | 4 | two-variant enum |
| `CloseCode` | 4 | 4 | `Custom(u16)` is the wide variant |
| `WsError` | 24 | 32 | enum: `(u8)` / `(u16)` for `InvalidOpcode`/`InvalidCloseCode` + `&'static str` for `ProtocolError`/`BadHandshake` |
| `Frame` | 40 | 64 | `(4 bools, Opcode, Option<[u8;4]>, Vec<u8>)` with niche packing |
| `Message` | 32 | 32 | `(MessageKind, Vec<u8>)` |
| `Handshake` | 96 | 128 | `(String, String, Vec<String>, Vec<String>)` |
| `ReassemblyOutput` | 40 | 96 | enum: `Pending` (0) / `Message(Message)` (32) / `Control(Frame)` (40) |
| `Reassembler` | 32 | 64 | `Option<Pending { kind, Vec<u8> }>` |
| `ExtensionOffer` | 48 | 64 | `(String, Vec<(String, Option<String>)>)` |
| `PermessageDeflateOffer` | 6 | 8 | `(bool, bool, Option<u8>, Option<u8>)` |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via
`cargo bloat --release --example min_ws --crates` against
`examples/min_ws.rs` (handshake + frame round-trip +
fragmented Text reassembly + `permessage-deflate` parse).
Regression budget: ≤ 5 % growth per minor is noise;
≥ 10 % stops the release.

| Version | Total binary `.text` | `grpcx-ws` contribution | Notes |
|---|---:|---:|---|
| v1.0 | 227.2 KiB | 7.0 KiB (3.1 % `.text`) | release profile (`lto=fat`, `panic=abort`, `strip=symbols`); aarch64-apple-darwin |

v1.0 break-down (`.text` share):

| Crate | Size | Share | Notes |
|---|---:|---:|---|
| `std` | 211.8 KiB | 93.2 % | unavoidable for `std` build |
| `min_ws` | 10.3 KiB | 4.5 % | example glue (handshake + frame + reassembly assertions) |
| **`grpcx_ws`** | **7.0 KiB** | **3.1 %** | L9 itself: framing + handshake + reassembly + extensions + inlined SHA-1/base64 |
| `[Unknown]` | 3.8 KiB | 1.7 % | linker glue |
| `__rustc` | 212 B | 0.1 % | runtime bits |

L9 sits between L7's pure-codec footprint (424 B / 0.2 %)
and L8's full parser+codegen footprint (27.1 KiB / 10.8 %)
— framing + handshake are sizeable but no heavy backend.
The inlined SHA-1 + base64 helpers are tens of bytes each
(small `process_block` + a tiny encode loop).
