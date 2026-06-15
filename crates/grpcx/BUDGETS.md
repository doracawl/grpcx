# `grpcx` Performance Budgets

Hot / warm / cold path budgets, enforced once backends
land (perf_gate wiring planned for v0.7+ once a baseline
exists). Project priority order is **perf > stability >
mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `tonic`
(`compare_tonic.rs`, lands v0.7+).

## v0.1 status

API skeleton only — no measurable hot path yet. Tables
below are templates for v0.7 to fill.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | `call_unary(channel, method, metadata, request)` end-to-end | v0.3 | Per-call wire encode + send + receive + decode |
| HOT | gRPC frame encode (5-byte header + payload) | v0.2 | Per-message wire-format cost |
| HOT | gRPC frame decode (length-prefixed read + payload) | v0.2 | Per-message decode cost |
| WARM | ALPN negotiation + transport selection | v0.6 | Per-connection setup cost |
| WARM | `Server::dispatch` lookup | v0.3 | Per-RPC method dispatch table lookup |
| COLD | `compare_tonic` head-to-head | v0.7 | Apples-to-apples wire + throughput bench |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_tonic`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public ADTs — `Status`,
`Metadata`, `Method`, `RpcError`. Numbers land v0.7.

Measured on aarch64-apple-darwin (16-byte align, 8-byte
pointers).

| Type | Size (bytes) | Bound | Notes |
|---|---:|---:|---|
| `StatusCode` | TBD | 4 | repr-default enum, 17 variants |
| `RpcKind` | TBD | 4 | 4-variant enum |
| `TransportKind` | TBD | 4 | 3-variant enum |
| `Status` | TBD | 32 | `(StatusCode, String)` |
| `Method` | TBD | 48 | `(String, String, RpcKind)` |
| `Metadata` | TBD | 32 | `Vec<(String, Vec<u8>)>` |
| `RpcError` | TBD | 64 | enum: `Status(Status)` is the wide variant |
| `Channel` | TBD | 32 | private; v0.3 fills shape |
| `Server` | TBD | 64 | private; v0.3 fills shape |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via
`cargo bloat --release --example min_grpcx --crates`
against `examples/min_grpcx.rs` (one unary RPC round-trip
over `grpcx/h3`; lands v0.3+).
Regression budget: ≤ 5 % growth per minor is noise;
≥ 10 % stops the release.

| Version | Total binary | `grpcx` contribution | Notes |
|---|---:|---:|---|
| v0.1 | n/a | n/a | API-only skeleton; `examples/min_grpcx.rs` lands v0.3 |

## e2e vs tonic (v0.7 GATING comparison, 2026-06-10)

Harness: `comparators/tonic-grpcx` — grpcx (gRPC/HTTP-3
over real UDP loopback, sans-IO drive on 2 threads) vs
tonic 0.12 (gRPC/HTTP-2 over real TCP loopback, tokio
multi-thread). Same echo workload, 500 warmup + 500 timed
serial unary calls, Apple M4 Max.

| payload | **grpcx-h3-udp** | tonic-h2-tcp | verdict |
|---|---|---|---|
| 16 B | **23.7 µs p50 / 35.6k RPS** | 33.5 µs / 29.3k | **grpcx +41% latency, +21% RPS** |
| 1 KiB | **34.2 µs p50 / 29.1k RPS** | 40.7 µs / 23.8k | **grpcx +19% / +22%** |
| 64 KiB | 505 µs / 1.9k | 69.3 µs / 14.2k | tonic 7.3× (see below) |
| (pure stack, mem loopback) | 7.0 µs / 141k RPS @16B | n/a | protocol-stack cost is 4.8× below tonic's whole-system cost |

**Small/medium payloads (the dominant gRPC shape): grpcx
wins end-to-end.** The 64 KiB loss is IO-layer plus
in-stack cost (mem lane does 64 KiB in ~200-230 µs).

**v0.12 update (2026-06-11)**: Darwin batch-datagram
syscalls (grpcx-io 1.2.0 `sendmsg_x`/`recvmsg_x`, ≤64
datagrams per syscall) landed in `UdpTransport` — same
machine, same run: 64 KiB 684.9 µs → **456.7 µs / 2 130
RPS** (+45 % RPS; the absolute numbers differ from the
v0.7 table above with machine load — compare within a
run). The remaining 64 KiB gap vs tonic is now in-stack
(mem lane ≈ 229 µs alone). Linux `sendmmsg` ships only
once verifiable on a Linux box.

**64 KiB stack pass 1 (2026-06-11, grpcx-quic 1.3.1 +
grpcx-h3 1.3.1)**: samply found 56 % of pure-stack
samples in memmove/malloc — crypto was only ~6 %. Four
cuts (L4 front-drain O(n²) → send cursor; L5 write_data
triple-copy → direct header emit; L5 receive DATA double
copy → in-place fast path; L4 packet buffer exact-size):
mem lane 229 → **160 µs**, UDP lane 457 → **382 µs /
2 585 RPS** (cumulative from the 685 µs v0.11 baseline:
**-44 %**). tonic still leads the lane (99-125 µs);
remaining after pass 1: per-datagram alloc in pump,
receive double-copy in L4 `decode_stream`, residual
grow-reallocs.

**64 KiB datagram-budget pass (2026-06-11, grpcx-quic +
grpcx-h3 1.4.0, grpcx 0.13.0)**: the fixed 1100-byte
1-RTT plaintext budget made every 64 KiB transfer ~60
datagrams. Now caller-configurable
(`set_datagram_budget`; default 1350 internet-safe;
loopback 9000 under Darwin's maxdgram 9216) — 8 packets
instead of 60. Steady state: UDP lane **~166 µs / 5.8k
RPS** (cumulative from 685 µs: **-76 %**, RPS 4×); pure
stack **~85 µs — below tonic's whole cleartext round
trip (~90-98 µs)**, while grpcx carries full QUIC
encryption. The protocol stack no longer loses; the
remaining UDP gap (~80 µs vs tonic) is the IO-drive
shape (two-thread spin/yield vs tokio event wakeups) —
owned by the v1.x multithread-reactor item, plus ACK
coalescing. Spin threshold 512 was measured WORSE than
64 (spinning recv syscalls contend with the peer
thread).

**Reactor park pass (2026-06-11, grpcx 0.14.0)**: socket
drive loops now park in L1's reactor (kqueue/epoll via
`DatagramTransport::wait_readable`) after the 64-cycle
spin budget, instead of yield-spinning. A/B (alternating
binaries, same machine): lane latency park ≈ yield within
noise on every payload — the serial lanes live inside the
spin budget, so the park rarely engages mid-call; **idle
server CPU drops ~144× (2.88 s → 0.02 s per 3 s idle,
`listen_h3_until`)**. Park thresholds 16 and 128 both
measured WORSE than 64. This run's steady state: 16 B
**32.5 µs vs tonic 43 (-25 %)**, 1 KiB **33.2 vs 54
(-38 %)**, 64 KiB 146.7 vs 89 (1.65×, pure stack 71.7 —
still below tonic's whole cleartext trip). Conclusion
recorded: the remaining 64 KiB gap is NOT idle-wait shape
— it is per-datagram IO + in-stack cost (≈71 µs UDP-mem
delta over 8+8 datagrams). Next lever: stack pass 2,
profile-driven. (An "ACK coalescing" lever was named here
originally — misrecord, since corrected: L4 carries no
ack machinery at all, lockstep-loopback semantics; there
are no ACK datagrams to coalesce.)

**Stack pass 2 + spin ledger (2026-06-11, quic/h3 1.4.1,
grpcx 0.14.1)**: profileudp's per-syscall ledger
(`CountingTransport`) attributed the 64 KiB UDP call:
66.8 EMPTY recv syscalls ≈ 41 µs (the spin loop), send
~27 µs + real recv ~10 µs (kernel datagram copies — the
structural UDP-vs-TCP cost on lo0: 8 datagrams against
tonic's single 64 KiB `write`), park ~26 µs (true wait).
Spin budget 64 → 4 (A/B: every lane improves, 16 B
-31 %; yesterday's "early park is worse" was load noise).
Five stack cuts (L4 send-side borrowed STREAM encoder,
L4 in-place STREAM receive, per-conn recv scratch,
`read_stream` swap handoff, L5 request-buffer recycling +
L10 two-write message frames): pure stack 81.7 → ~59-68
µs; remaining profile ≈ 20 % AEAD floor + fragmented
1-2 % semantic-boundary copies. Steady state within-run:
**16 B 32.3 vs 42.0 (-23 %), 1 KiB 33.2 vs 53.6 (-38 %),
64 KiB 134 vs 88.7 (1.51×, was 7.3× at v0.11; RPS 5×)**.
The remaining 64 KiB gap is dominated by Darwin lo0
per-datagram kernel cost (maxdgram 9216 ⇒ 8 packets) —
levers left: raise maxdgram (system config, not code),
or further stack cuts below the AEAD floor. ERRATUM
recorded: "ACK coalescing" was a misrecord — L4 has no
ack machinery (lockstep-loopback semantics; reliability
is an unscoped correctness feature).

**The maxdgram experiment (2026-06-12, grpcx 0.14.2)**:
raising `net.inet.udp.maxdgram` 9216 → 65535 (sysctl,
root) lets the budget probe (`comparators .../budget.rs`)
pick ~65 000 ⇒ a 64 KiB call is 2 data packets instead of
8. Within-run: 64 KiB **111-113 µs vs tonic ~90 → 1.26×**
(was 1.51× at the default sysctl; 7.3× at v0.11). Ledger
@65000: send 27 → 10.2 µs, recv-full 10 → 4.4 (IO -60 %);
the remaining mass is the peer-processing wait (~55 µs
park = server stack time + ~20 µs two-way kernel wake
tax) + ~40 µs own-stack. Spin 48 re-A/B'd at this shape:
still worse than 4 everywhere — the wake tax is physical;
empty recvs can't buy it back. Shipped half: batch slots
re-laid 32×16 KiB → 8×64 KiB (same 512 KiB) so a raised
sysctl never truncates a batch receive. **Deploy
guidance: large-payload loopback/jumbo deployments should
raise `maxdgram` (or size the budget to path MTU);
small/medium lanes are unaffected.** Default-sysctl
numbers remain the comparison baseline.

Bugs this harness caught (the point of the gating item):
1. L4 packed ALL pending stream data into one datagram
   (MTU violation — invisible on mem loopback, instant
   failure on real UDP). Fixed: MTU-budgeted chunking.
2. L4+L5 leaked one dead stream entry per RPC; per-pump
   sweeps made the whole stack O(calls²). Fixed:
   close_request/close_stream + soft-close reaping.
3. Drive loops serialized one datagram per scheduling
   round-trip. Fixed: batch-drain + spin-then-yield.

## MEM (aarch64-darwin, const-asserted in src/sizes.rs)

| ADT | Size (B) | Bound |
|---|---|---|
| `Method` | 56 | 56 |
| `RpcKind` / `TransportKind` | 1 | 1 |
| `Status` | 32 | 32 |
| `StatusCode` | 4 | 4 |
| `Metadata` | 24 | 24 |
| `RpcError` | ≤96 | 96 |
| `MessageFrame` | 32 | 32 |

## BINSIZE

| Example | Bytes | Notes |
|---|---|---|
| `min_grpcx` (release, stripped) | 368 656 (~360 KiB) | one unary echo over ws-mux — the no-TLS transport, so the binary carries L9+L10 only. The H/3 path adds the rustls/ring stack (see grpcx-crypto BUDGETS). |
