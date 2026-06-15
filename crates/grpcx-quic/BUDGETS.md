# `grpcx-quic` Performance Budgets

Hot / warm / cold path budgets, enforced by `tests/perf_gate.rs`
once backends land (perf_gate wiring planned for v0.6+ once a
baseline exists). CI skips these with `--skip _under_budget`;
run locally on a quiet machine.

Project priority order is **perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `quinn` (`compare_quinn.rs`, lands v0.7).

## v1.0 status (project v4.0)

L4 closed at v1.0 (2026-06-08). The full PERF tier (cycle counts
on the steady-state HOT path, `compare_quinn` harness) defers to
v1.x — same charter as L1 / L2 / L3 perf passes. MEM + BINSIZE
land here.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | packet decrypt (AEAD-open + HP unmask + header parse) | v0.2 | One incoming packet through the steady-state 1-RTT path |
| HOT | packet encrypt (header build + HP mask + AEAD-seal) | v0.2 | One outgoing packet on the steady-state 1-RTT path |
| HOT | frame decode (`STREAM` / `ACK`) on a borrowed buffer | v0.3 | Per-frame parsing in `FrameRef<'a>` borrow-only form |
| HOT | frame encode (`STREAM` / `ACK`) into a caller buffer | v0.3 | Per-frame emit on the send path |
| HOT | stream read / write (one `read` / `write` call) | v0.4 | Per-call userspace cost (no syscall) |
| HOT | loss recovery PTO arming + ACK processing | v0.5 | Per-ACK steady-state |
| HOT | congestion controller `on_ack` / `on_loss` (CUBIC) | v0.5 | Per-event cost |
| WARM | handshake — initial → handshake → 1-RTT | v0.4 | One full 1-RTT handshake |
| WARM | endpoint demux (`Mutex<BTreeMap<ConnectionId, _>>` lookup) | v0.4 | Per-packet endpoint-level dispatch |
| WARM | stream open / close | v0.4 | Per-stream lifecycle |
| WARM | path validation (PATH_CHALLENGE / PATH_RESPONSE) | v0.4 | One challenge round-trip |
| COLD | endpoint bind + UDP socket setup | v0.4+ | Once per endpoint |
| COLD | retry token mint + verify | v0.4 | Anti-amplification path |
| COLD | version negotiation | v0.4 | Per first-packet unsupported-version |
| COLD | connection close (graceful + abort) | v0.4 | Per-connection teardown |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_quinn`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public ADTs — `ConnectionId`,
`StreamId`, `Error`, `EcnCounts`, `LongHeader`, `ShortHeader`,
`PacketHeader`, `Frame`. Methodology mirrors L1 / L2 / L3
`BUDGETS.md` (static `size_of` const-asserts in `src/sizes.rs`
plus a runtime report test that prints the values).

`QuicConnection` itself is intentionally not yet const-asserted
— it holds a `rustls::quic` connection inside, whose size is set
by the upstream wrapper and not by L4. A v1.x perf pass will pin
it once the self-impl L3 lands.

Per-packet allocation budget: target is **zero** on the
steady-state 1-RTT path. v0.6's loopback driver currently
allocates per-packet for `aad.to_vec()` + `packet.to_vec()`
inside `dispatch_datagram`; the borrow-only `FrameRef<'a>` plus
the caller-owned-buffer pump shape land in v1.x.

Numbers below were measured on aarch64-apple-darwin (16-byte
alignment, 8-byte pointers). The `const _: () = assert!(…)`
guards in `src/sizes.rs` permit ≤ each row's listed size with
slack; tightening the bounds is a v1.x knob.

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `ConnectionId` | 21 | 24 | `[u8; 20]` + `u8 len`, packed |
| `StreamId` | 8 | 8 | `u64` newtype |
| `Error` | 1 | 8 | tag-only enum, no payloads |
| `EcnCounts` | 24 | 24 | three `u64` counters |
| `LongHeader` | 88 | 128 | Vec'd token + Vec'd CIDs inside |
| `ShortHeader` | 28 | 32 | inline CID |
| `PacketHeader` | 88 | 144 | enum over Long / Short |
| `Frame` | 80 | 128 | enum over 21 variants (max-variant cost) |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via `cargo bloat
--release --example min_quic --crates` against the canonical
`examples/min_quic.rs` (open one connection, send one stream
message, close). Regression budget: ≤ 5 % growth per minor is
noise; ≥ 10 % stops the release.

| Version | Total binary | `grpcx-quic` contribution | Notes |
|---|---:|---:|---|
| v1.0 | 1.5 MiB | 27.1 KiB (3.1 % `.text`) | release profile (`lto=fat`, `panic=abort`, `strip=symbols`); aarch64-apple-darwin |

v1.0 break-down by crate (`.text` share, top contributors):

| Crate | Size | Share | Notes |
|---|---:|---:|---|
| `std` | 296.1 KiB | 34.1 % | unavoidable for `std` build |
| `rustls` | 252.0 KiB | 29.0 % | TLS — L3 interim wrapper |
| `ring` | 138.4 KiB | 15.9 % | crypto provider |
| `rcgen` | 40.4 KiB | 4.6 % | dev-only: self-signed cert mint in the example |
| `webpki` | 35.3 KiB | 4.1 % | TLS chain validation |
| `min_quic` | 28.7 KiB | 3.3 % | example glue |
| `grpcx_quic` | 27.1 KiB | 3.1 % | L4 itself |
| `grpcx_crypto` | 11.9 KiB | 1.4 % | L3 wrapper |

`grpcx_quic` is the second-smallest non-glue crate in the stack
— roughly 1/9th the size of `rustls` and 1/5th of `ring`. The
v1.x self-impl crypto pass will collapse the `rustls` + `ring`
contributions; the L4 contribution is already at the BINSIZE
ceiling we set in `docs/v4-scope.md`.
