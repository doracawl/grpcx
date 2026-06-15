# `grpcx-h3` Performance Budgets

Hot / warm / cold path budgets, enforced by `tests/perf_gate.rs`
once backends land (perf_gate wiring planned for v0.7+ once a
baseline exists). CI skips these with `--skip _under_budget`;
run locally on a quiet machine.

Project priority order is **perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is the `h3` crate (`compare_h3.rs`, lands v0.7).

## v1.0 status (project v5.0)

L5 closed at v1.0 (2026-06-09). The full PERF tier (cycle counts
on the steady-state HOT path, `compare_h3` harness) defers to
v1.x — same charter as L1 / L2 / L3 / L4 perf passes. MEM +
BINSIZE land here.

API surface complete — no implementation gaps remain. Stabilisation
includes:
 This file fixes the path taxonomy so each v0.x checkpoint
fills its rows as it lands. All `Observed` / `Headroom` cells are
TBD until the corresponding backend exists.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | frame decode on a borrowed buffer (DATA + HEADERS path) | v0.2 | Per-frame parsing on the request hot path |
| HOT | frame encode into a caller buffer (DATA + HEADERS path) | v0.2 | Per-frame emit on the send path |
| HOT | QPACK static-table encode (one header pair) | v0.3 | Per-header indexed-name lookup |
| HOT | QPACK static-table decode (one header pair) | v0.3 | Per-header decode hop |
| HOT | request-stream `write_data` / `read_data` (one call) | v0.4 | Per-call userspace cost (no syscall) |
| HOT | QPACK dynamic-table insert + lookup | v0.6 | Per-header dynamic-table hop |
| WARM | SETTINGS exchange (one handshake round-trip) | v0.5 | Per-connection |
| WARM | GOAWAY emit + drain | v0.5 | Per-connection teardown |
| WARM | control stream open + classify (uni-stream typing dispatch) | v0.4 | Per-control-stream lifecycle |
| WARM | request open (`send_request` + first HEADERS frame) | v0.5 | Per-request |
| COLD | QPACK dynamic-table eviction | v0.6 | Per-eviction-event |
| COLD | server push (PUSH_PROMISE + push-stream open) | v0.6 | Per-push (gated by an explicit acceptance test) |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_h3`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public types — `Connection`,
`RequestStream`, `Header`, `Settings`, `StatusCode`, `Error`.
Methodology mirrors L1 / L2 / L3 / L4 `BUDGETS.md` (static
`size_of` const-asserts plus a runtime report test). Numbers
land v0.7.

Per-frame allocation budget: target is **zero** on the
steady-state DATA path. v0.x ships an owned `Frame` enum for
clarity; the borrow-only `FrameRef<'a>` lands in v1.x.

Measured on aarch64-apple-darwin (16-byte align, 8-byte
pointers). The `const _: () = assert!(…)` guards in
`src/sizes.rs` permit ≤ each row's listed size with slack.

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `Header` | 48 | 64 | two `Vec<u8>` (24 each, 16-byte align) |
| `Settings` | 32 | 64 | three `Option<u64>` + `bool` |
| `StreamType` | 16 | 24 | enum with one `u64` variant |
| `Frame` | 56 | 64 | enum over 10 variants |
| `EncoderInstruction` | 64 | 96 | largest variant has two `Vec<u8>` |
| `DynamicEntry` | 48 | 64 | same shape as `Header` |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via `cargo bloat
--release --example min_h3 --crates` against the canonical
`examples/min_h3.rs` (one GET round-trip; lands v0.5). Regression
budget: ≤ 5 % growth per minor is noise; ≥ 10 % stops the
release.

| Version | Total binary | `grpcx-h3` contribution | Notes |
|---|---:|---:|---|
| v1.0 | 1.26 MiB | 20.8 KiB (2.3 % `.text`) | release profile (`lto=fat`, `panic=abort`, `strip=symbols`); aarch64-apple-darwin |

v1.0 break-down (top contributors of `.text`):

| Crate | Size | Share | Notes |
|---|---:|---:|---|
| `std` | 302.2 KiB | 33.6 % | unavoidable for `std` build |
| `rustls` | 252.0 KiB | 28.0 % | TLS — L3 interim wrapper |
| `ring` | 138.4 KiB | 15.4 % | crypto provider |
| `rcgen` | 40.4 KiB | 4.5 % | dev-only: example cert mint |
| `webpki` | 35.3 KiB | 3.9 % | TLS chain validation |
| `min_h3` | 31.8 KiB | 3.5 % | example glue |
| `grpcx_quic` | 28.1 KiB | 3.1 % | L4 |
| `grpcx_h3` | 20.8 KiB | 2.3 % | L5 itself |
| `grpcx_crypto` | 11.9 KiB | 1.3 % | L3 wrapper |

L5 is the third-smallest non-glue crate in the stack — ~3/4 the
size of L4 and ~1/12th of `rustls`. The v1.x self-impl crypto
pass will collapse the `rustls` + `ring` contributions; L5's own
contribution is already at the BINSIZE ceiling we set.
