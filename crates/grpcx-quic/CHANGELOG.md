# Changelog

All notable changes to `grpcx-quic` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.9.1] below)

## [1.9.1] - 2026-06-12

**Cancel-frame loss recovery** — closes the departure
recorded at 1.9.0: RESET_STREAM / STOP_SENDING now ride
each packet's `SentRecord` and re-queue (front) on
packet-threshold loss or PTO harvest, per RFC 9000 §13.3.
KAT: a cancel burst eaten on the wire is re-sent via PTO
and the peer still learns
(`lost_cancel_frames_are_retransmitted_via_pto`).

## [1.9.0] - 2026-06-12

**Eager cancel.** The stream-reset plane goes live (the
frames existed in the codec since v4; the connection
dropped them):

### Added
- `cancel_stream(sid, code)` — RESET_STREAM for our send
  half (pending data + retransmit spans purged, final
  size = bytes already on the wire) + STOP_SENDING for the
  peer's. `take_reset_streams()` surfaces ended streams to
  the upper layer.
- Receive: RESET_STREAM drops recv state and reaps once
  both halves are dead; STOP_SENDING abandons the send
  half and answers RESET_STREAM (RFC 9000 §3.5). Late
  in-flight data for reset sids is dropped via a bounded
  recently-reset ring, not re-registered.
- DEPARTURE (recorded): RESET_STREAM/STOP_SENDING are
  one-shot best-effort, NOT loss-recovered (RFC 9000
  §13.3 wants retransmission) — a lost cancel means the
  peer wastes work, which was the pre-1.9 status quo for
  every cancel.

## [1.8.1] - 2026-06-12

Perf patch (64KiB stack pass 3, profile-attributed): 1-RTT
frames now encode DIRECTLY into the packet buffer
(`begin_short_packet` / `seal_short_packet` replace the
`plaintext`-staging + `build_short_packet` copy) — one
alloc + one full-payload memmove per packet gone; AAD is
borrowed via `split_at_mut` instead of an owned copy. No
API change; A-B-A: pure-stack 64KiB −3.3%, e2e UDP lane
−7% (with the h3 1.7.1 drain fix).


## [1.8.0] - 2026-06-12

**Link liveness, atomized.** The transport now owns the
"is the link alive?" question (the caller's drive loop no
longer guesses from idle counters):

### Added
- Keepalive: a link with no inbound datagrams for the
  keepalive interval (default 2.5 s,
  `set_keepalive_us`) queues a PING — its ACK is inbound
  traffic, which is the liveness proof; the PTO machinery
  retries a lost probe.
- Idle timeout: no inbound datagrams for the idle window
  (default 10 s, `set_idle_timeout_us`) →
  `pump_at` returns `Error::Timeout` (the link is dead).
  A handshake against a peer that never answers is
  bounded by the same window from its first CRYPTO send.
- Gap forgiveness: a pump gap wider than the keepalive
  interval means the driver wasn't watching (a blocking
  caller between calls) — the quiet window re-anchors
  instead of firing a false death on the next pump.
- All of it rides the 1.6.0 sans-IO clock: untimed pumps
  never fire liveness, like every other timer.

## [1.7.0] - 2026-06-12

**Reliability phase 3 — persistent congestion + CUBIC.**
Closes the v4 bootstrap charter item 5: "CUBIC default,
NewReno fallback".

### Added
- Persistent congestion (RFC 9002 §7.6): packets counted
  lost — threshold-declared AND PTO-harvested (the p2 PTO
  design removes records at harvest, so a blackout
  produces only those) — feed a send-time span tracker,
  reset by any forward-progress ACK. A span past
  3 × PTO collapses cwnd to the minimum window (2 ×
  budget) and re-enters slow start (ssthresh untouched,
  per the RFC pseudocode).
- CUBIC (RFC 9438), **the new default**: β = 0.7, fast
  convergence (17/20 plateau cut on back-to-back
  reductions), cubic regrowth around w_max with the
  t + srtt look-ahead and a 1.5 × cwnd target clamp, and
  the TCP-friendly W_est region — which governs at
  loopback RTTs (K sits seconds out), so lossless lanes
  behave exactly as before. Integer fixed-point
  throughout (`icbrt` by binary search, curve in
  ms³/10¹⁰ units; no floats, `no_std` kept).
- `CongestionAlgorithm { Cubic, NewReno }` +
  `set_congestion_algorithm()` — NewReno keeps the exact
  phase-2 arithmetic as the chartered fallback.

### Acceptance
- KATs: icbrt exactness property + edges, cubic curve
  points hand-computed, K for a 100-segment plateau.
- Gold: total-blackout collapse to exactly 2 × budget,
  then byte-exact recovery + slow-start regrowth on link
  restore; NewReno fallback run over the lossy shape.
  The phase-2 suite (7 tests) green under the new
  default.

## [1.6.0] - 2026-06-12

**Reliability phase 2 — the sans-IO time surface.** The
three loss shapes packet-threshold detection cannot see
(flight tails, lost ACKs, handshake-plane loss) now
recover on timers instead of surfacing as the caller's
~32 ms stall. Time is caller-injected: a never-timed
connection (`pump`/`pump_with` only) keeps its clock at 0,
never fires a timer, and behaves exactly like 1.5.

### Added
- `pump_at(incoming, allow_pure_ack, now_us)` — the timed
  pump; the internal clock is monotonic (`max`), so timed
  and untimed pumps interleave safely.
- RTT estimation (RFC 9002 §5, exact): smoothed RTT /
  rttvar / min RTT from ACKs that newly cover the
  largest-acknowledged packet; `smoothed_rtt_us()` getter.
- PTO / tail-loss probe (RFC 9002 §6.2, adapted): on
  expiry the oldest unacked packet's spans rejoin the
  retransmit queue ahead of newer lost spans (a packet
  with nothing retransmittable probes with PING — the
  peer's re-ACK covers its full received ranges, which
  recovers a lost ACK). Backoff doubles per unanswered
  probe; pre-first-sample PTO uses kInitialRtt = 5 ms
  (RFC says 333 ms — lab-stack default sized to the 32 ms
  drive-loop stall budget).
- Handshake-plane CRYPTO retransmit: per-level outbound
  logs blindly re-sent in full (offset 0) on the PTO
  timer until the handshake is CONFIRMED; the server now
  actually sends HANDSHAKE_DONE (RFC 9001 §4.1.2, queued
  on establishment, re-queued if lost) and the client
  confirms on receiving it. No per-level ACK machinery —
  deliberate: the receive side dedups, so blind resend
  converges over a phase that lasts two round trips.
- CRYPTO receive reassembly: duplicate/overlap trim +
  out-of-order parking per level (was: hard error on any
  gap). Undecryptable packets at a keyless level are
  dropped (the lost/reordered-flight shape) instead of
  killing the connection; decrypt failures under
  installed keys still error.
- NewReno congestion control (RFC 9002 §7 minimal):
  initial window 10 × budget, slow start + congestion
  avoidance, one cwnd halving per recovery episode, floor
  2 × budget. The gate applies to NEW stream data and
  DATAGRAM frames; retransmits/ACKs/probes/handshake
  planes are exempt. No reduction on PTO (persistent-
  congestion detection deferred). `cwnd_bytes()` getter;
  `set_datagram_budget` re-arms the initial window (the
  budget is the CC unit).
- `next_wakeup_us()` — earliest armed deadline for
  drivers that want to bound a park; fixed park slices
  (L10's 500 µs) merely add their slice to probe latency.

### Acceptance
- 7 gold tests (`integ_pto_recovery.rs`, deterministic
  synthetic clock): tail loss, lost ACK, lost server
  handshake flight, lost client Finished (recovered via
  HANDSHAKE_DONE), RTT-estimator KATs, cwnd-limited first
  burst, loss-shrinks-window A/B. Phase 1's four loss
  gold tests unchanged-green.

## [1.5.0] - 2026-06-12

**Reliability phase 1 — the 1-RTT plane stops being
lockstep-only.** ACK generation/processing, retransmit
buffering, and packet-threshold loss recovery (RFC 9002
§6.1.1, kPacketThreshold = 3). A lossy path no longer
stalls a call: deterministic loss / duplication /
reordering suites recover byte-exact streams.

### Added
- ACK frames: received-PN range tracking; ACKs piggyback
  on any outgoing packet immediately, and a PURE-ACK
  packet is emitted only on a truly idle pump (see
  `pump_with`) — per-received-packet pure ACKs measured
  +25 % on the 16 B lane and were gated out.
- `pump_with(incoming, allow_pure_ack)` — `pump` keeps
  its signature/semantics (`allow = incoming.is_none()`);
  layered pumps (L5 flushes its own writes with a second
  idle pump per datagram) pass `false` mid-chain.
- Send-side `SentRecord` ledger + per-stream three-cursor
  send buffer (acked / on-wire / unsent): bytes are
  retained until acknowledged (the retransmit source) and
  released wholesale — no front-drain on the hot path.
- Packet-threshold loss declaration → retransmit queue,
  drained ahead of new data by the 1-RTT builder.
- Receive-side offset reassembly: duplicate trimming,
  out-of-order parking, FIN honored only at final size.
- `close_stream` / stream reaping now wait for full
  acknowledgment, not just on-wire.

### Performance
- Net cost ≈ 0 after ACK gating: 16 B 33.4 µs, 1 KiB
  33.9, 64 KiB 134-135 — all within noise of the
  pre-reliability baseline; packet counts identical
  (ledger: 10/10 per 64 KiB call vs 17/19 before gating).

### Explicitly phase 2 (needs the sans-IO time surface)
- PTO timers / tail-loss probes (a lost flight tail or a
  lost ACK still relies on the caller's stall timeout),
  handshake-plane CRYPTO retransmit, RTT estimation,
  congestion control.

## [1.4.1] - 2026-06-11

**Stack pass 2 (perf patch, no API change).** Profile-driven
copy cuts on the 64 KiB hot path:

- Send side: queue slices encode straight into the packet
  plaintext via a borrowed-payload STREAM encoder
  (`encode_stream_frame_into`, lockstep-KAT'd against
  `encode_frame`) — kills the per-packet `to_vec` round
  trip through an owned `Frame`.
- Receive side: short-packet STREAM frames parse in place
  and extend `recv_buf` directly (no owned-`Frame`
  bounce); `decrypt_short_packet` split out of the old
  `parse_short_packet`.
- `dispatch_datagram`: per-packet mutable copy now reuses
  a per-connection scratch buffer (one malloc per packet
  gone).
- `read_stream`: zero-copy buffer swap when the caller's
  buffer is drained (capacity recycles between L4 and the
  caller).

## [1.4.0] - 2026-06-11

**Configurable datagram budget.** The 1-RTT plaintext
budget was a 1100-byte constant — every 64 KiB transfer
became ~60 datagrams, paying per-packet sealing, header
protection, allocation, ACKs, and scheduling 60 times.

### Added
- `Connection::set_datagram_budget(plaintext_budget)` —
  caller-asserted against path MTU. Default raised
  1100 → 1350 (the QUIC-ecosystem conservative
  internet value); loopback links can go to ~9000
  (Darwin `net.inet.udp.maxdgram` = 9216).

### Results (with grpcx-h3 1.4.0 + grpcx 0.13.0)
- L10 64 KiB UDP loopback lane @9000: 382 → **~166 µs /
  5.8k RPS** (cumulative from the 685 µs v0.11 baseline:
  **-76 %**, RPS 4×).

## [1.3.1] - 2026-06-11

**Perf patch — the 64 KiB profile pass (no API change).**
samply over L10's pure-stack 64 KiB unary lane found 56 %
of samples in memmove/malloc; the worst single line lived
here.

### Fixed
- `StreamState::pending_send` was front-drained with
  `Vec::drain(..take)` per datagram — every 1100-byte
  chunk memmoved the entire remaining tail (O(n²): a
  64 KiB send shifted ~2 MB). Replaced with a
  `send_cursor` read offset; the buffer resets once
  fully drained.
- `build_short_packet` grew its output through 3-4
  reallocs per packet (~60 packets per 64 KiB transfer);
  now sized exactly up front. The 1-RTT plaintext
  assembly buffer is also pre-sized to the budget.

### Results
- L10 pure-stack 64 KiB lane: 214.7 → ~140 µs/call
  (combined with the grpcx-h3 1.3.1 cuts).

## [1.3.0] - 2026-06-10

**MTU-budgeted packetisation + stream lifecycle** —
layer-up patch for L10 v0.7, both items surfaced by the
first real-UDP e2e run (the in-memory loopback had hidden
them).

### Fixed
- `build_one_rtt_datagram` packed ALL pending stream data
  into a single datagram — a 64 KiB write produced one
  ~65 KB datagram (RFC 9000 §14 violation; instant
  EMSGSIZE on real UDP sockets). Stream data is now
  chunked to a 1100-byte plaintext budget per datagram
  (encrypted short packet stays under the 1200-byte path-
  MTU floor); `pump` loops the builder until the queues
  drain. RFC 9221 DATAGRAM frames stay indivisible: packed
  while they fit, oversized ones go out alone.

### Added
- `Connection::close_stream` — soft-close: the entry is
  reaped once its send queue fully drains onto the wire.
  Without it every RPC leaked one dead `StreamState` and
  per-stream sweeps degraded linearly with call count
  (O(calls²) whole-stack behaviour found by the L10
  comparator harness).

## [1.2.0] - 2026-06-09

**RFC 9221 DATAGRAM frame plumbing.** Additive 1.2.0 minor —
wires the existing `Frame::Datagram` codec (in place since
v0.4) into the connection state machine so the upper layer
(L5 `grpcx-h3` 1.2.0 + L6 `grpcx-wt` 0.5.0) can use H/3
datagrams. Same dependency pattern L4 1.0 → 1.1.0 followed
during L5 v0.5. See [../../REFACTOR-v6-v0.5-datagrams.md](../../REFACTOR-v6-v0.5-datagrams.md).

### Added
- `QuicConnection::send_datagram(payload) -> Result<(), Error>`
  — queue one DATAGRAM frame for the next 1-RTT outbound packet.
  Rejects with `Error::BadFrame` when 1-RTT keys aren't installed
  yet (datagrams have no protected level before then).
- `QuicConnection::recv_datagram() -> Option<Vec<u8>>` — pop the
  next inbound DATAGRAM payload.
- `Connection` grew `outbound_datagrams` (drained in
  `build_one_rtt_datagram`) + `inbound_datagrams` (populated by
  the `Frame::Datagram` arm of `handle_frame`).

### Tests
- `tests/integ_datagrams.rs` (4 cases): client→server,
  server→client, multiple FIFO, send-before-1-RTT rejected.

### Deferred to v1.x
- Transport-parameter negotiation for
  `max_datagram_frame_size` (RFC 9221 §3 TP id `0x20`). Lands
  with the broader transport-params parser.

### Gate
- `cargo test -p grpcx-quic`: existing + 4 new integ. All green.
- `cargo test --workspace --skip _under_budget`: 398 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [1.1.0] - 2026-06-09

**Additive minor — uni-stream open + peer-stream enumeration.**
Required by L5's `grpcx-h3` v0.5 Connection state machine to
open the H/3 control + QPACK encoder/decoder uni-streams and to
discover incoming server-push / control / QPACK streams on the
peer side. No breaking changes to the 1.0.0 surface.

### Added
- `QuicConnection::open_uni() -> StreamId` — open a new
  locally-initiated unidirectional stream. RFC 9000 §2.1
  stream-id bit 1 = 1 marks uni; bit 0 matches our `Side`.
  First client-initiated uni id = 2; first server-initiated
  uni id = 3; both +4 per subsequent open.
- `QuicConnection::accept_uni() -> Option<StreamId>` /
  `QuicConnection::accept_bi() -> Option<StreamId>` — pop the
  next peer-initiated uni / bi stream id from the FIFO. Streams
  are classified the first time they appear in any inbound
  STREAM frame (via the new `note_peer_initiated` helper);
  locally-initiated and pseudo-CRYPTO ids are filtered out.

### Tests
- `tests/integ_uni_streams.rs` — 4 cases:
  - Client opens uni → server accepts uni (id 2 round-trip with
    payload + FIN).
  - Server opens uni → client accepts uni (id 3 round-trip).
  - Mixed bi + uni: server's `accept_bi` / `accept_uni` queues
    are independent and the right ids land in each.
  - Sequential `open_uni` increments by 4 (RFC 9000 §2.1 stride).

### Gate
- `cargo test -p grpcx-quic` (skip flaky perf_gate): 88 / 88
  passing (84 pre-1.1.0 + 4 new uni-stream integ).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [1.0.0] - 2026-06-08

**Project v4.0 release** — L4 closes. Stabilisation drop:
coverage gate cleared, fuzz harness in place, MEM + BINSIZE
numbers landed in `BUDGETS.md`, `compare_quinn` harness compiles.
See [../../REFACTOR-v4-v0.7-close-out.md](../../REFACTOR-v4-v0.7-close-out.md)
and [../../REFACTOR-v4.0-release.md](../../REFACTOR-v4.0-release.md).

### Added
- `src/sizes.rs` — `size_of` const-asserts on the public ADTs
  (`ConnectionId`, `StreamId`, `Error`, `EcnCounts`, `LongHeader`,
  `ShortHeader`, `PacketHeader`, `Frame`) + a runtime
  `report_internal_sizes` test that prints them. Static guards
  against silent type-size bloat.
- `src/__fuzz_helpers.rs` (`#[doc(hidden)]`) — byte-driven drivers
  over the public surface (`drive_varint`, `drive_frame`,
  `drive_packet_header`). Consumed by `fuzz/fuzz_targets/*`.
- `fuzz/` (cargo-fuzz subcrate, workspace-excluded) — three
  targets: `varint_ops`, `frame_ops`, `header_parse`. All build
  clean on nightly + smoke-run zero crashes (10 M+ execs
  cumulative on the smoke pass).
- `examples/min_quic.rs` — minimal loopback driver (handshake to
  1-RTT + one stream round-trip) for BINSIZE measurement.
- `benches/compare_quinn.rs` — head-to-head bench harness vs
  `quinn-proto`'s wire-format primitives. Codec-only (no
  connection state); end-to-end perf comparison defers to the
  v1.x perf pass.
- `tests/edge_cases.rs` (31 cases) — rejection-path coverage on
  the public surface: frame oversize-CID rejects + Datagram
  no-length decode + unknown frame type + ACK oversize
  range_count regression; packet empty-buf / oversize-CID /
  bad-pn-len / fixed-bit-zero rejects; protection AES-256
  hp-suite arm + iv-wrong-length + apply/remove rejects;
  initial-packet `_with_initial_dcid` round-trip + short-buf
  parse rejection.

### Fixed
- **frame: ACK decode unbounded `range_count` (OOM on adversarial
  input).** `decode_ack` did `Vec::with_capacity(range_count)`
  where `range_count` was a 62-bit varint. A crafted ACK frame
  claiming `range_count ≈ 2^58` against a tiny buffer aborted the
  process with OOM (caught by `fuzz/frame_ops` in <1k execs). Fix:
  cap `range_count` by remaining buffer length (each gap+range
  pair needs ≥ 2 varint bytes) before allocating. Regression
  pinned in `tests/edge_cases.rs::frame_ack_oversize_range_count_rejected`.

### Cleanup
- Removed dead v0.1 stub types (`Endpoint`, `Connecting`, the v0.1
  `Connection` stub, `SendStream`, `RecvStream`, `ClientConfig`,
  `ServerConfig`) from `src/lib.rs`. The v0.6+ working surface
  (`QuicConnection`) is the public driver.
- Removed never-reached helpers (`_varint_len_unused`,
  `_write_varint_unused`) from `connection.rs`.
- Replaced `build_initial_packet`'s lingering import in
  `connection.rs` with the actually-used
  `build_initial_packet_with_initial_dcid`.

### Gate
- `cargo test --workspace` 83 tests passing (47 from v0.6 + 30
  new edge_cases + 5 in-crate sizes/connection tests + 1 fuzz
  smoke).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo doc --workspace --no-deps` zero-warning.
- `cargo fmt --check` clean.
- `cargo llvm-cov -p grpcx-quic --summary-only
  --ignore-filename-regex '__fuzz_helpers\.rs'` reports **96.88 %
  lines, 98.91 % functions, 91.72 % regions** — over the 95 %
  gate set in `docs/v4-scope.md`. (Workspace all-in with the
  fuzz-only driver included is 94.39 % lines; the exclusion is
  the explicit fallback the trigger doc allows for, since
  `__fuzz_helpers.rs` is exercised by the `fuzz/` subcrate, not
  the main test suite.)
- `cargo +nightly fuzz build` clean on all three targets
  (aarch64-darwin native triple).
- `BUDGETS.md` MEM + BINSIZE tables filled.

### Deferred to v1.x
- Cross-OS `compare_quinn` publish + `PERFORMANCE.md` (same
  charter as L1 / L2 / L3 perf pass — harness here, numbers later).
- 24 h cumulative fuzz long-run.
- Loss recovery + congestion control (RFC 9002).
- Real UDP socket binding via `grpcx-io`.
- 0-RTT, connection migration, coalesced packets, Retry, Version
  Negotiation.
- `quic-interop-runner` against external peers.

## [0.6.0] - 2026-06-08

**Connection state machine + full handshake to 1-RTT +
bidirectional STREAM round-trip**. The first L4 checkpoint where
the wire is actually carrying a QUIC connection end-to-end. See
[../../REFACTOR-v4-v0.6-connection.md](../../REFACTOR-v4-v0.6-connection.md).

### Added
- `QuicConnection` (`src/connection.rs`) — owns a
  `grpcx_crypto::quic_handshake::QuicEndpoint` + per-encryption-
  level keys (Initial / Handshake / 1-RTT) + per-level packet
  number counters + a stream map.
- `QuicConnection::new_client` / `new_server` — build the L4
  Connection from a `grpcx_crypto::TlsConfig`.
- `pump(incoming) -> Vec<Vec<u8>>` — receive-and-emit step.
  Caller hands in one inbound datagram (or `None` for an idle
  flush); we return the outbound datagrams the next pump produces.
- `open_bi()` / `write_stream(sid, data, fin)` /
  `read_stream(sid, &mut out) -> (n, fin)` — the bidirectional
  stream surface. Plus `peek_stream` / `streams_have_data` test
  helpers.
- `build_initial_packet_with_initial_dcid(...)` — variant of
  `build_initial_packet` that takes the Initial-key seed (RFC
  9001 §5.2 client-DCID) as an explicit parameter, separate from
  the packet's on-wire DCID. Required for the server → client
  direction once the peers re-address each other.
- Re-exports: `QuicConnection`,
  `build_initial_packet_with_initial_dcid`.

### Tests
- `tests/integ_loopback.rs` (1 case) — full intra-process QUIC
  acceptance:
  1. Mint a self-signed Ed25519 cert via rcgen.
  2. Build client + server `QuicConnection`s.
  3. Pump packets through an in-memory bus until both sides
     reach 1-RTT (`is_established() == true`).
  4. Client opens a bidi stream, writes `b"GET /index"` with
     FIN; server reads it back with byte-equality + FIN flag.
  5. Server writes `b"HTTP/3 200 OK"` with FIN; client reads it
     back.
- Total `grpcx-quic` test count: **47 across 6 KAT/integration
  files** (v0.2 12 + v0.3 6 + v0.4 27 + v0.5 1 + v0.6 1).

### Dependencies
- `grpcx-crypto` bumped to `1.2.0` (additive minor adding
  `quic_handshake::QuicEndpoint` + `QuicKeys` + per-level key
  promotion via `QuicKeyChange`).

### Deferred (v0.7+)
- Loss recovery + congestion control (RFC 9002).
- Flow control (per-stream + connection-level).
- Connection migration (RFC 9000 §9).
- 0-RTT support.
- Coalesced packets within a single datagram (RFC 9000 §12.2).
- Retry + Version Negotiation packet codecs.
- Real UDP socket binding (the v0.6 loopback runs over an
  in-memory bus; binding to `grpcx-io`'s UDP socket type is
  v1.x).

## [0.5.0] - 2026-06-07

Initial packet build + parse helpers (composes v0.2-v0.4 primitives)
and an L3 ↔ L4 end-to-end integration test carrying a real TLS 1.3
ClientHello through a QUIC Initial packet round-trip. See
[../../REFACTOR-v4-v0.5-initial-packet.md](../../REFACTOR-v4-v0.5-initial-packet.md).

### Partition reshuffle
- `docs/v4-scope.md`'s v0.5 was "loss recovery + congestion
  control". That moves into a v1.x perf-pass slot. v0.5 instead
  retires the highest-risk integration (L4 ↔ L3 packet protection
  + handshake bytes) before the connection state machine in v0.6.

### Added
- `initial` module — `Side` enum (`Client` / `Server`) +
  `build_initial_packet` + `parse_initial_packet`. Composes
  `quic::initial_secret` + HKDF-Expand-Label + `quic::key_iv_hp`
  (L3) with header codec + frame codec + packet protection (L4
  v0.2-v0.4) into a usable Initial-level packet pair.
- `build_initial_packet` honours the RFC 9000 §14.1 first-flight
  padding (caller passes `padded_to: usize`; encoder grows the
  PADDING-frame run to land at the target wire length).
- `parse_initial_packet` does HP-unmask → AEAD-decrypt → header
  decode → frame decode in one call.
- v0.5 fixes pn_len = 4 bytes and the AEAD suite at AES-128-GCM
  (RFC 9001 §5.1 mandates it at the Initial level). The v0.6
  Connection state machine will pick PN length dynamically.

### Tests
- `integ_initial.rs` (1 case) — end-to-end:
  - Mint a self-signed cert via rcgen.
  - Drive `grpcx_crypto::ClientHandshake` to produce ClientHello.
  - `build_initial_packet` the ClientHello with the RFC 9001 §A
    reference DCID (`0x8394c8f03e515708`) + 1200-byte padding.
  - `parse_initial_packet` the wire bytes back.
  - Assert: ClientHello bytes byte-equal across the round-trip;
    header version / pn / pn_len / dcid match; padding frames
    are present.
- Total `grpcx-quic` test count: **46 across 5 KAT files** (v0.2
  12 + v0.3 6 + v0.4 27 + v0.5 1).

### Dependencies
- `rcgen = { workspace = true }` added as `dev-dependency` (same
  exception class as L3's loopback test).

### Deferred (v0.6+)
- Connection state machine — endpoint demux + accept / connect
  flow + multi-packet handshake loop + 1-RTT key promotion +
  stream multiplexing.
- Retry / Version Negotiation packet codecs.
- `Side` auto-discovery from the parsed long-header type (v0.5
  takes it as a parameter; v0.6 will pick the right side based on
  whether the packet is for us as client or server).

## [0.4.0] - 2026-06-07

Frame codec — RFC 9000 §19 + RFC 9221 DATAGRAM. Owned `Frame`
enum + `encode_frame` / `decode_frame`. See
[../../REFACTOR-v4-v0.4-frame-codec.md](../../REFACTOR-v4-v0.4-frame-codec.md).

### Added
- `frame` module — `Frame` enum (one variant per RFC 9000 §19
  frame type plus `Datagram` from RFC 9221) + `EcnCounts` struct
  + `decode_frame` / `encode_frame` / `frame_varint_len`.
- Coverage: PADDING / PING / ACK (with + without ECN) /
  RESET_STREAM / STOP_SENDING / CRYPTO / NEW_TOKEN / STREAM (all
  8 wire variants via `(fin, has_offset)` flags) / MAX_DATA /
  MAX_STREAM_DATA / MAX_STREAMS (bidi+uni) / DATA_BLOCKED /
  STREAM_DATA_BLOCKED / STREAMS_BLOCKED (bidi+uni) /
  NEW_CONNECTION_ID / RETIRE_CONNECTION_ID / PATH_CHALLENGE /
  PATH_RESPONSE / CONNECTION_CLOSE (transport+application) /
  HANDSHAKE_DONE / DATAGRAM.

### Tests
- `kat_frame.rs` (27 cases) — round-trip per frame variant +
  truncated-CRYPTO rejection + unknown-frame-type rejection +
  DATAGRAM `0x30` (no-length) wire-form decode (encoder always
  emits `0x31`).
- Total `grpcx-quic` test count: **45 across 4 KAT files** (v0.2
  12 + v0.3 6 + v0.4 27).

### Deferred (v0.5+)
- Borrow-only `FrameRef<'a>` for the zero-alloc hot path — design
  intent #1 of the L4 charter; lands when the connection state
  machine in v0.5 surfaces the actual hot path.
- Retry packet codec + Version Negotiation packet codec — still
  pending; both move into v0.5 alongside the endpoint demux they
  need.

## [0.3.0] - 2026-06-07

Packet protection — RFC 9001 §5.3 (AEAD) + §5.4 (header
protection). Consumes the new `grpcx-crypto 1.1` minor that adds
`quic::header_protect_mask`. See
[../../REFACTOR-v4-v0.3-packet-protection.md](../../REFACTOR-v4-v0.3-packet-protection.md).

### Added
- `protection` module — `AeadSuite` enum (`Aes128Gcm` /
  `Aes256Gcm` / `ChaCha20Poly1305`); `protect_payload` /
  `unprotect_payload` (in-place AEAD with QUIC nonce derivation
  `iv_base XOR right-aligned PN`); `apply_header_protection` /
  `remove_header_protection` (in-place first-byte + PN mask, with
  long/short header first-byte mask widths per RFC 9001 §5.4).
- `AeadSuite::hp_suite() -> grpcx_crypto::quic::HpSuite` — 1:1
  pairing helper.
- `AeadSuite::tag_len() -> usize` — 16 for all v1 suites.

### Tests
- `kat_protection.rs` (6 cases) — payload AEAD round-trip per
  suite × 3 + HP round-trip × 2 (long + short headers) + RFC 9001
  §A.2 client-side composed round-trip using the RFC-quoted key /
  IV / HP key for the reference cid `0x8394c8f03e515708`.
- Total `grpcx-quic` test count: **18 across 3 KAT files** (v0.2
  12 + v0.3 6).

### Dependencies
- `grpcx-crypto` bumped to `1.1.0` (additive minor adding
  `quic::HpSuite` + `header_protect_mask`).

### Deferred (v0.4+)
- Retry packet codec (needs integrity tag via AEAD).
- Version Negotiation packet codec.
- Full byte-level RFC 9001 §A.2 + §A.3 + §A.4 replay (lands when
  the connection state machine in v0.4 builds the full Initial-
  packet path).

## [0.2.0] - 2026-06-07

First implementation drop — the **unprotected** wire forms: RFC
9000 §16 varint codec + packet-header encode/decode (Long header
Initial / 0-RTT / Handshake + Short header 1-RTT). AEAD packet
protection + header-protection are deferred to v0.3 so this
checkpoint stays digestible. See
[../../REFACTOR-v4-v0.2-packet-header.md](../../REFACTOR-v4-v0.2-packet-header.md).

### Added
- `varint` module — RFC 9000 §16 variable-length integer codec
  (`write_varint` / `read_varint` / `varint_len`), crate-private.
- `packet` module — `LongHeader` / `ShortHeader` / `PacketHeader`
  / `LongHeaderType` + `encode_header` / `decode_header` /
  `varint_field_len`. Decode borrows the input buffer for
  DCID/SCID/token slices; owned `ConnectionId` is the inline
  `[u8; 20]` already in the public surface.
- Short header decode takes a caller-supplied `dcid_len` —
  RFC 9000 §17.3.1, the wire doesn't carry length for 1-RTT (the
  endpoint knows it per-connection).

### Tests
- `kat_varint` (5 cases) — RFC 9000 §A.1 examples (1/2/4/8-byte
  forms) + shortest-form round-trip across boundary values +
  truncated-buffer + overlong-value rejection.
- `kat_packet_header` (7 cases) — Long Initial / Handshake / 0-RTT
  + Short 1-RTT (with key_phase) + Short spin-bit + truncated
  long header rejected + PN-truncation round-trip.

### Deferred (v0.3+)
- AEAD packet protection (RFC 9001 §5.3) — needs the L3
  `grpcx-crypto::quic::key_iv_hp`-derived keys; lands v0.3.
- Header protection (RFC 9001 §5.4) — needs an AES-128-ECB / ChaCha
  mask op that the v1.0 `grpcx-crypto` doesn't expose yet; will
  add `quic::header_protect_mask` as a `grpcx-crypto` 1.1 minor
  and consume it from v0.3 here.
- Retry packet codec — Retry integrity tag needs AEAD; lands v0.4
  alongside the connection state machine.
- Version Negotiation packet codec — lands v0.4 alongside
  endpoint demux.

## [0.1.0] - 2026-06-07

L4 bootstrap — `grpcx-quic` crate skeleton, API trait design,
design docs synced. The four-doc bootstrap pattern that L1 + L2 +
L3 followed (`ROADMAP § Lx` + `docs/vN-scope.md` +
`REFACTOR-vN-v0.1-bootstrap.md`; no `DEPS_AUDIT` exception needed
— L4 has no external runtime dep). See
[../../REFACTOR-v4-v0.1-bootstrap.md](../../REFACTOR-v4-v0.1-bootstrap.md).

### Added
- `src/lib.rs` API skeleton — `Endpoint`, `Connecting`,
  `Connection`, `SendStream`, `RecvStream`, `ClientConfig`,
  `ServerConfig`, `ConnectionId`, `StreamId`, `Error`. Every
  item carries a doc comment; bodies are `unimplemented!()` —
  the v0.2-v0.5 checkpoints land the packet framing → frame
  codec → connection state machine → loss recovery
  implementation one piece at a time.
- `BUDGETS.md` — performance budget taxonomy (HOT / WARM / COLD)
  for the hot paths that will land in v0.2-v0.5. Numbers TBD
  until the corresponding backend exists.

### Dependencies
- `grpcx-io` (L1) + `grpcx-rt` (L2) + `grpcx-crypto` (L3). No
  external runtime dep — charter consistency restored after the
  L3 wrapper exception. `quinn` is **not** a dep; L4 is the
  `quinn` replacement.
