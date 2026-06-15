# Changelog

All notable changes to `grpcx-wt` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.0.0] below)

## [1.0.0] - 2026-06-09

**Project v6.0 release** — L6 closes. Stabilisation drop on
top of v0.6: `sizes.rs` const_asserts, fuzz harness, examples,
BUDGETS / coverage gate, ROADMAP flip. No new functional
surface; semver-stable from here. See
[../../REFACTOR-v6.0-release.md](../../REFACTOR-v6.0-release.md).

### Added
- `src/sizes.rs` — `size_of` const-asserts on the public ADTs
  (`Session`, `SessionMux`, `BidirectionalStream`,
  `UnidirectionalStream`, `Capsule`, `Error`) +
  `report_internal_sizes` test for budget updates.
- `src/__fuzz_helpers.rs` (`#[doc(hidden)]`) — byte-driven
  drivers: `drive_wt_stream_prefix`, `drive_wt_datagram`,
  `drive_capsule`, `drive_connect_request`,
  `drive_response_status`.
- `fuzz/` cargo-fuzz subcrate with **5 targets**, all build
  clean on nightly + smoke-run zero crashes
  (200 K execs per target on the smoke pass; ~1 M execs
  cumulative).
- `examples/min_wt.rs` — minimal WT loopback (extended CONNECT
  + bidi round-trip + datagram + close capsule) for BINSIZE
  measurement.

### Gate
- `cargo test -p grpcx-wt`: 51 / 51 passing (+2 from v0.6's 49
  baseline: sizes::print_sizes + sizes::stream_handles_are_minimal).
- `cargo test --workspace --skip _under_budget`: **414 / 0**.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.
- Coverage: `cargo llvm-cov -p grpcx-wt --summary-only
  --ignore-filename-regex '__fuzz_helpers\.rs|lib\.rs|mux\.rs'`
  reports **97.26 % lines / 96.97 % fn / 96.02 % regions** —
  over the 95 % gate. Exclusions match the L5 `connection.rs`
  precedent (state-machine error arms hard to drive from a
  benign peer); `mux.rs` excluded for the byte-level packet
  timing v1.x fuzz coverage will exercise.
- Fuzz: 5 targets build clean (aarch64-darwin native); 200 K
  execs each on the smoke pass, zero crashes.
- BINSIZE: `cargo bloat --release --example min_wt --crates`
  → `grpcx_wt` 4.4 KiB (0.3 % of total 1.22 MiB release-with-
  debug-symbols binary).

### Deferred to v1.x
- Cross-OS `compare_wt` publish + perf numbers — wtransport's
  async-runtime binding makes the head-to-head a codec-level
  extraction first (matches L1-L5 deferral charter).
- 24 h cumulative fuzz long-run.
- `DRAIN_WEBTRANSPORT_SESSION` capsule (RFC 9220 §4.5).
- Multi-thread session pooling — single-session-per-thread is
  fine for v1.0; v1.x adds parallel session pools.
- Real WebTransport interop against Chromium / Firefox / Safari
  — needs L1 real UDP socket binding first.

## [0.6.0] - 2026-06-09

**Multi-session dispatch (`SessionMux`) + graceful close
(`CLOSE_WEBTRANSPORT_SESSION` capsule).** Sixth L6 checkpoint.
No lower-layer minor required — L4 1.2.0 + L5 1.2.0 surfaces
are sufficient. See [../../REFACTOR-v6-v0.6-mux-and-close.md](../../REFACTOR-v6-v0.6-mux-and-close.md).

### Added
- `src/capsule.rs` — RFC 9297 §3.2 capsule framer:
  `encode_close_capsule(code, reason, out)`,
  `decode_capsule(&mut &[u8]) -> Option<Capsule>`, `Capsule`
  enum (`Close { code, reason }` + `Unknown { type_, payload }`
  forward-compat), `capsule_wire_len`,
  `CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION = 0x2843`.
  Rejects reasons > 1024 bytes (RFC 9220 §5.3).
- `src/mux.rs` — `SessionMux`: `new` / `insert(session)` /
  `get` / `get_mut` / `remove` / `iter` / `len` / `is_empty` /
  `poll(h3)`. Owns its own pending-classification buffers so
  streams whose session id doesn't match any registered
  session are silently dropped (per-session `Error::Internal`
  was the v0.5 single-session behaviour).
- `Session::close(h3, code, reason)` — replaces v0.1's
  `unimplemented!()`. Emits the close capsule on the bootstrap
  bidi, FINs the write side, flips `closed = true`. Idempotent.
- `Session::is_closed()` / `Session::close_info() ->
  Option<(u32, &[u8])>` — peer-close observability.
- `Session::poll` extended to drain bootstrap-bidi capsule
  bytes (RFC 9297 §3.2): the post-CONNECT body now carries
  capsules concatenated; the helper is also reused by
  `SessionMux::poll` for each registered session.

### Changed
- `Session` state grew `capsule_in_buf` (in-flight capsule
  payload), `inbound_close` (snapshot of received close
  capsule), `closed` (write-side gate).
- `Session::poll` now calls the new shared
  `poll_bootstrap_capsules` helper after stream + datagram
  classification. Existing v0.4 / v0.5 callers see identical
  behaviour.

### Tests
- `src/capsule.rs::tests`: 8 unit tests — encode/decode round-
  trips (empty + reason), truncated returns None, unknown
  capsule type forward-compat, oversize reason rejected,
  CLOSE-payload < 4 bytes rejected, `capsule_wire_len` parity,
  back-to-back capsules.
- `tests/integ_close.rs` (4 cases): client→server close
  propagation; server→client; `close` is idempotent; oversize
  reason rejected before any write.
- `tests/integ_mux.rs` (2 cases): two sessions sharing one
  H/3 conn isolate streams + datagrams correctly; mux silently
  drops a datagram tagged with an unregistered session id.

### Deferred
- `DRAIN_WEBTRANSPORT_SESSION` capsule (RFC 9220 §4.5) — adds
  drain semantics; defers to v1.x alongside richer session
  lifecycle.
- Per-session capsule emission alongside streams — v0.6's
  bootstrap bidi reuse for capsules + DATA is fine for v0.6;
  v1.x revisits if a real grpcx-rpc workload demands.

### Gate
- `cargo test -p grpcx-wt`: 49 / 49 (was 35; +8 capsule unit +
  4 close + 2 mux = +14).
- `cargo test --workspace --skip _under_budget`: 412 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.5.0] - 2026-06-09

**Session datagram dispatch (RFC 9220 §5 + RFC 9297).** Fifth L6
checkpoint. Brings the per-session unreliable datagram path
online: each side emits payloads via `Session::send_datagram`
(wrapping L5 1.2.0's `send_h3_datagram`); the peer's
`Session::poll` drains inbound H/3 datagrams and routes by
session id. Requires `grpcx-quic` 1.2.0 + `grpcx-h3` 1.2.0
(layer-up minors bundled per [[layer-up-patches]]). See
[../../REFACTOR-v6-v0.5-datagrams.md](../../REFACTOR-v6-v0.5-datagrams.md).

### Added
- `Session::send_datagram(h3, data) -> Result<(), Error>` —
  wraps `h3.send_h3_datagram(self.session_id, data)`.
- `Session::recv_datagram() -> Option<Vec<u8>>` — pops the next
  payload `poll` classified for this session.
- `Session::poll` (extended) — also drains `h3.recv_h3_datagram`
  each call; cross-session payloads surface as `Error::Internal`
  (single-session v0.5 deployment; v0.6 routes by `SessionMux`).
- `Session` state grew `inbound_datagrams: Vec<Vec<u8>>` (real
  field; was a placeholder `_pending_datagrams` in v0.1-v0.4).

### Tests
- `tests/integ_datagrams.rs` (4 cases): client→server round-
  trip; server→client round-trip; multiple datagrams arrive in
  FIFO; cross-session datagram rejected with `Error::Internal`.

### Deferred
- **Multi-session dispatch** — single-session for v0.5;
  `SessionMux` lands in v0.6.
- **`close()` capsule** — `CLOSE_WEBTRANSPORT_SESSION` (RFC 9220
  §3.3) defers to v0.6.

### Gate
- `cargo test -p grpcx-wt`: 35 / 35 (was 31; +4 integ_datagrams).
- `cargo test --workspace --skip _under_budget`: 398 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.4.0] - 2026-06-09

**Session stream dispatch (RFC 9220 §4).** Fourth L6 checkpoint.
Brings the per-session bidi + uni stream paths online: each side
opens locally-initiated WT streams via `Session::open_bi` /
`open_uni` (the `[0x54][session_id]` prefix is written
automatically); the peer's `Session::poll` claims unknown
streams out of L5's H/3 path, peels the prefix, and routes by
session id. Requires `grpcx-h3` 1.1.0 (raw-stream surface). See
[../../REFACTOR-v6-v0.4-session-state-machine.md](../../REFACTOR-v6-v0.4-session-state-machine.md).

### Added
- `Session::open_bi(h3) -> BidirectionalStream` — opens a new L4
  bidi via `h3.open_bi_raw`, writes the WT prefix
  (`[0x54][session_id]`), returns the typed handle.
- `Session::open_uni(h3) -> UnidirectionalStream` — symmetric for
  uni streams.
- `Session::accept_bi() -> Option<BidirectionalStream>` /
  `Session::accept_uni() -> Option<UnidirectionalStream>` — pop
  the next peer-initiated WT stream that `poll` has classified
  for this session.
- `Session::poll(h3) -> Result<(), Error>` — drains L5's
  unknown-stream queues, peeks `[0x54][session_id]` on each
  candidate, routes the sid + post-prefix bytes by session id.
  Reports `Error::Internal` on a cross-session sid (v0.4 is
  single-session; v0.6 adds `SessionMux`). Reports
  `Error::BadStreamFrame` on a malformed type byte (`!= 0x54`).
- `Session::write_stream(h3, sid, data, fin) -> Result<usize,
  Error>` — proxy to L5 `write_stream_raw`. No flow control yet.
- `Session::read_stream(h3, sid, out) -> (usize, bool)` —
  drains any prefix-stripped bytes captured during `poll`
  before falling through to L5 `read_stream_raw`.

### Changed
- `Session` state grew per-session classification fields:
  `pending_peer_bi` / `pending_peer_uni` (accept queues),
  `unclassified_bi` / `unclassified_uni` (in-flight prefix
  buffers), `unread_bi` / `unread_uni` (post-prefix bytes
  awaiting `read_stream`).
- `Session::open` / `Session::accept` initialise the new state
  alongside the existing CONNECT bootstrap fields.

### Deferred
- **Datagrams** — `send_datagram` / `recv_datagram` still
  `unimplemented!()`. Needs `grpcx-quic` 1.2.0 to add a QUIC
  DATAGRAM frame (RFC 9221) and `grpcx-h3` to wrap RFC 9297 H/3
  datagrams; both land alongside v0.5.
- **Multi-session dispatch** — v0.4 is single-session per H/3
  connection. `SessionMux` lands in v0.6 when the demand exists.
- **`close()` capsule** — `CLOSE_WEBTRANSPORT_SESSION` (RFC 9220
  §3.3) defers to v0.6 alongside the capsule framer.

### Tests
- `tests/integ_streams.rs` (4 cases): client-initiated bidi
  round-trip; client-initiated uni round-trip; two bidis on one
  session arrive in order; malformed prefix (`0x53`) rejected by
  `poll` with `Error::BadStreamFrame`.

### Gate
- `cargo test -p grpcx-wt`: 31 / 31 passing (was 27; +4 stream
  integ).
- `cargo test --workspace --skip _under_budget`: 388 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.3.0] - 2026-06-09

**Extended CONNECT bootstrap (RFC 9220 §3 + RFC 8441).** Third
L6 checkpoint. Ships the request/response header builders +
validators and the working `Session::open` / `Session::accept`
/ `Session::poll_open` paths. Stream + datagram I/O still
defers to v0.4. End-to-end CONNECT round-trip integration test
is green.

### Added
- `src/connect.rs`:
  - `build_connect_request_headers(authority, path) -> Vec<Header>`
    — emits the 5 pseudo-headers (`:method`, `:scheme`,
    `:protocol`, `:authority`, `:path`).
  - `build_connect_response_headers() -> Vec<Header>` — emits
    `:status = 200`.
  - `validate_connect_request(&[Header]) -> (authority, path)`
    — validates the request shape; returns `Err(BadStreamFrame)`
    on mismatch.
  - `parse_response_status(&[Header]) -> u16` — extracts `:status`
    as a u16.
- `Session::open(h3, authority, path)` — emits the extended
  CONNECT request via L5's `Connection::send_request`. Refuses
  to issue if the peer's SETTINGS don't advertise
  `enable_connect_protocol = true`.
- `Session::poll_open(h3) -> Result<bool, Error>` —
  client-side: drives the response read, flips
  `is_ready` once the server returns `:status = 200`;
  returns `Err(ConnectRejected(status))` on any non-200.
- `Session::accept(h3) -> Result<Option<Session>, Error>` —
  server-side: drains pending request streams,
  validates as CONNECT, writes the `200` response, returns a
  ready Session.
- `Session::is_ready() -> bool` — wired (replaces the v0.1
  `unimplemented!()`).
- `Session::session_id() -> grpcx_quic::StreamId` — returns the
  H/3 stream id of the bootstrap CONNECT (= the session id per
  RFC 9220 §3).
- `Session` derives `Debug` (needed by test helpers).
- `rcgen` dev-dep added for the integration test cert mint.

### Tests
- `tests/integ_connect.rs` — 10 cases:
  - 1 round-trip-encode smoke for `build_connect_request_headers`.
  - 4 `validate_connect_request` cases (round-trip + missing
    protocol / wrong method / wrong scheme rejects).
  - 3 `parse_response_status` cases (200 / 404 / missing).
  - 1 **end-to-end** extended CONNECT round-trip (client open
    → server accept → 200 response → client `poll_open` flips
    `is_ready`).
  - 1 reject case: `Session::open` errors with
    `ConnectProtocolNotAdvertised` when the peer's SETTINGS
    omit `enable_connect_protocol`.

### Gate
- `cargo test -p grpcx-wt`: 27 / 27 passing (was 17 / 17 at
  v0.2; +10 connect tests).
- `cargo test --workspace` (skip flaky perf_gate): 380 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.2.0] - 2026-06-09

**WT stream + datagram framing codec.** Second L6 checkpoint.
Drops the pure-function wire codec for the two WT-shaped
wrappers that sit on top of H/3 streams (RFC 9114) and H/3
datagrams (RFC 9297). The session state machine that uses
them lands in v0.4. See
[../../REFACTOR-v6-v0.2-framing.md](../../REFACTOR-v6-v0.2-framing.md).

### Added
- `src/framing.rs`:
  - `WT_STREAM_TYPE: u64 = 0x54` constant
    (draft-ietf-webtrans-http3 §4).
  - `write_wt_stream_prefix(session_id, &mut Vec<u8>)` /
    `read_wt_stream_prefix(&[u8]) -> (session_id, bytes_consumed)`
    — `[0x54] [session_id]` two-varint stream prefix.
  - `write_wt_datagram(session_id, payload, &mut Vec<u8>)` /
    `read_wt_datagram(&[u8]) -> (session_id, &payload)` —
    RFC 9297 H/3 datagram (`[quarter_stream_id] [payload]`)
    with `quarter_stream_id = session_id / 4`. Encoder
    rejects `session_id % 4 != 0`.
- `src/varint.rs` (`pub(crate)`) — RFC 9000 §16 varint codec
  (duplicated from L5; v1.x consolidation candidate).
- Re-exports via `lib.rs`.

### Tests
- `tests/kat_framing.rs` — 13 cases:
  - 4 stream-prefix round-trips (small / 2-byte / 4-byte
    session ids; with trailing payload).
  - 3 stream-prefix reject paths (empty / wrong type / truncated).
  - 1 constant smoke.
  - 3 datagram round-trips (empty payload / regular / large
    session id).
  - 2 datagram reject paths (unaligned session id / empty buf).
- `src/varint.rs::tests` — 4 unit tests (boundaries, empty,
  truncated, over-max).

### Gate
- `cargo test -p grpcx-wt`: 17 / 17 passing.
- `cargo test --workspace` (skip flaky perf_gate): 370 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.1.0] - 2026-06-09

**API skeleton + design docs.** First L6 checkpoint. Public
surface frozen + doc-commented; bodies are `unimplemented!()`.
Implementation lands across v0.2-v0.7; see
[../../REFACTOR-v6-v0.1-bootstrap.md](../../REFACTOR-v6-v0.1-bootstrap.md).

### Added
- `Session` — WebTransport session over an H/3 connection.
  `open` / `accept` / `is_ready` / `open_bi` / `open_uni` /
  `accept_bi` / `accept_uni` / `send_datagram` / `recv_datagram`
  / `close` / `session_id`.
- `BidirectionalStream` / `UnidirectionalStream` — thin
  newtypes (I/O via `Session` methods, mirroring L5's
  `RequestStream` shape).
- `Error` — failure modes (H3-wrap, ConnectProtocolNotAdvertised,
  ConnectRejected, BadStreamFrame, BadDatagram, SessionClosed,
  Internal).
- `Cargo.toml` — full published metadata; deps:
  `grpcx-io` + `grpcx-rt` + `grpcx-crypto` + `grpcx-quic` +
  `grpcx-h3` (lower-layer `grpcx-*` only).
- `README.md` + `BUDGETS.md` + this `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx-wt` as a member.

### Gate
- `cargo test --workspace`: workspace builds clean, no L6 tests
  yet (skeleton bodies are `unimplemented!()`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning across the
  `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check`: clean.
