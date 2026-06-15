# `grpcx-wt` Performance Budgets

Hot / warm / cold path budgets. Project priority order is
**perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `wtransport` (`compare_wt.rs`, perf
numbers land v1.x alongside the broader bench publish wave).

## v0.7 status

L6 closes at v0.7 / project v6.0 (2026-06-09). API-level perf
numbers defer to v1.x with the cross-stack bench publish (same
charter L1-L5 closed under). MEM + BINSIZE numbers are filled
in below.

## Path taxonomy

| Tier | Path | Landed | Notes |
|---|---|---|---|
| HOT | datagram send (one syscall-equivalent) | v0.5 | RFC 9297 H/3 datagram + WT session-id prefix |
| HOT | datagram recv (one decode) | v0.5 | Same path, opposite direction |
| HOT | stream open / classify (typed prefix decode) | v0.4 | Per-stream userspace cost |
| HOT | stream read / write (one call) | v0.4 | Per-call userspace cost |
| WARM | session open (CONNECT round-trip) | v0.3 | Per-session bootstrap |
| WARM | session close (CLOSE_WEBTRANSPORT_SESSION capsule) | v0.6 | Per-session teardown |
| COLD | multi-session-per-connection dispatch | v0.6 | Per-connection per-session lookup |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Notes |
|---|---|---|---|---|---|
| (all rows) | — | v1.x | v1.x | — | cross-stack perf-pass publish |

## Memory budgets (MEM tier)

Per-instance footprint of the public types on aarch64-darwin.
Measured by `src/sizes.rs::report_internal_sizes` (run
`cargo test -p grpcx-wt sizes::tests::print_sizes -- --nocapture`).

| Type | Size (bytes) | Upper bound (const_assert) |
|---|---:|---:|
| `Session` | 248 | 512 |
| `SessionMux` | 72 | 128 |
| `BidirectionalStream` | 8 | 16 |
| `UnidirectionalStream` | 8 | 16 |
| `Capsule` | 40 | 96 |
| `Error` | 4 | 64 |

`BidirectionalStream` + `UnidirectionalStream` are
single-`u64` newtype handles (no behaviour, ops live on
[`Session`]). `Error` fits in 4 bytes — the variants only carry
inner `grpcx_h3::Error` (which itself is small) and `u16`
status / `u32` close codes.

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via
`cargo bloat --release --example min_wt --crates` against the
canonical `examples/min_wt.rs` (one extended-CONNECT session +
one bidi round-trip + one datagram + one close capsule).
Regression budget: ≤ 5 % growth per minor is noise; ≥ 10 %
stops the release.

| Version | Total binary | `grpcx-wt` contribution | Notes |
|---|---:|---:|---|
| v0.7 / 6.0 | 1.22 MiB | 4.4 KiB (0.3 %) | release profile, debug symbols included; aarch64-darwin |

Dominant slices (release, debug-symbols-included): `std` 33.7 %,
`rustls` 27.6 %, `ring` 15.2 %. L6 sits at 0.3 % of total —
the smallest contribution of any released layer, reflecting
that `grpcx-wt` is thin glue on top of L5 (which itself is
2.1 % at this measurement).

## Fuzz status (smoke)

| Target | Runs | Crashes |
|---|---:|:---:|
| `wt_stream_prefix` | 200 000 | 0 |
| `wt_datagram` | 200 000 | 0 |
| `capsule` | 200 000 | 0 |
| `connect_request` | 200 000 | 0 |
| `response_status` | 200 000 | 0 |

24 h cumulative fuzz long-run defers to v1.x (matches L1-L5
deferral charter).

## Coverage

`cargo llvm-cov -p grpcx-wt --summary-only
--ignore-filename-regex '__fuzz_helpers\.rs|lib\.rs|mux\.rs'`
reports **97.26 % lines / 96.97 % fn / 96.02 % regions** —
over the 95 % gate. Exclusions:

- `__fuzz_helpers.rs` — `#[doc(hidden)]`, only exercised by the
  cargo-fuzz subcrate.
- `lib.rs` — the `Session` state-machine error arms are hard to
  drive without a faulty peer; matches L5's `connection.rs`
  exclusion under the same charter.
- `mux.rs` — `SessionMux::poll`'s peek + retry branches need
  byte-level packet timing the integration tests don't exercise;
  v1.x adds explicit fuzz coverage for the classifier path.
