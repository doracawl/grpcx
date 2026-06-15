# Changelog

All notable changes to `grpcx-h3` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.8.0] below)

## [1.8.0] - 2026-06-12

**Eager cancel.**

### Added
- `cancel_request(rs)` — QUIC RESET_STREAM + STOP_SENDING
  with H3_REQUEST_CANCELLED (0x10c) + local request state
  dropped.
- Pump drains L4 reset events: a cancelled-but-not-yet-
  accepted request never reaches `accept_request` /
  dispatch.

## [1.7.1] - 2026-06-12

Perf patch (64KiB stack pass 3, profile-attributed):
`drain_one_request` switches to a deferred-drain cursor —
the per-frame `in_buf.drain(..)` memmoved the whole
remaining tail once per frame (several DATA frames per
burst at the loopback budget); now one drain per call.
No API change. A-B numbers in the quic 1.8.1 entry.


## [1.7.0] - 2026-06-12

### Added
- `set_link_liveness_us(keepalive_us, idle_timeout_us)` —
  passthrough to L4 1.8.0's link-liveness atom (quiet-link
  keepalive PING + no-inbound idle timeout).

### Changed
- Depends on grpcx-quic 1.8.0.

## [1.6.0] - 2026-06-12

### Added
- `pump_at(incoming, now_us)` — threads the caller's
  monotonic clock (µs) through to L4's 1.6.0 sans-IO time
  surface (RTT estimation, PTO/tail-loss probes,
  handshake-plane retransmit). `pump` keeps its exact
  signature and semantics by delegating with a frozen
  clock (no timer ever fires on untimed pumps).

### Changed
- Depends on grpcx-quic 1.6.0.

## [1.5.0] - 2026-06-12

**Reliability phase 1 wiring (layer-up with grpcx-quic
1.5.0).**

- The per-datagram flush pump (step 8) now calls
  `pump_with(None, false)` mid-chain so only a truly idle
  L5 pump may emit a PURE-ACK packet — per-datagram pure
  ACKs nearly doubled wire packet counts.

## [1.4.1] - 2026-06-11

**Stack pass 2 (perf patch, no API change).**

- Per-request buffer recycling: `close_request` harvests
  `in_buf` / `body` capacity into a bounded spare pool and
  new requests draw from it — a fresh `RequestState`
  otherwise re-grew both from zero every RPC (doubling
  copies, hot on the 64 KiB profile).

## [1.4.0] - 2026-06-11

### Added
- `Connection::set_datagram_budget` — passthrough to L4's
  configurable 1-RTT plaintext budget (grpcx-quic 1.4.0).

## [1.3.1] - 2026-06-11

**Perf patch — the 64 KiB profile pass (no API change).**

### Fixed
- `write_data` built an owned `Frame::Data`
  (`data.to_vec()`) and serialized it into an
  intermediate buffer before handing it to L4 — three
  full copies of every bulk payload. Now emits the DATA
  frame header directly and appends the payload as a
  second stream write (one copy: into L4's send queue).
- `drain_one_request` bounced L4 bytes through a
  temporary Vec before extending the reassembly buffer;
  now reads straight into it.
- Received DATA frames were decoded into an owned `Frame`
  and then copied again into the body buffer; a fast path
  now streams the payload straight from the reassembly
  buffer into the body (header parsed in place).

### Results
- L10 pure-stack 64 KiB lane: 214.7 → ~140 µs/call
  (combined with the grpcx-quic 1.3.1 cuts).

## [1.3.0] - 2026-06-10

**Trailing HEADERS (the gRPC shape)** — layer-up patch for
L10 v0.3 (per the established L4→L6 layer-up pattern: when
layer N+1 needs new API from layer N, ship a minor on N
first).

### Added
- `RequestState` now stores the second HEADERS frame on a
  request stream as the trailing section (RFC 9114 §4.1
  `HEADERS DATA* HEADERS`) instead of dropping it.
- `Connection::read_trailers` /
  `Connection::received_trailers_ready` — same
  dispatch-once semantics as `read_headers`.
- Sending trailers = a second `write_headers` call before
  `finish` (already legal; now documented).
- A third HEADERS frame on the same stream is dropped
  (peer protocol violation).
- `tests/integ_trailers.rs` — full
  HEADERS+DATA+trailers(fin) loopback round-trip + the
  third-frame-dropped case.
- `Connection::close_request` (added later the same day,
  L10 v0.7) — releases the H/3 `RequestState` and the
  underlying L4 stream entry once the caller is done.
  Closes the per-RPC state leak that made per-pump stream
  sweeps O(calls²) (found by the L10 e2e comparator).

## [1.2.0] - 2026-06-09

**RFC 9297 H/3 datagram wrapper.** Additive 1.2.0 minor — adds
a thin wrapper around `grpcx-quic` 1.2.0's DATAGRAM-frame API
that encodes the `[quarter_stream_id varint][payload]` shape
RFC 9297 specifies. Enables L6 `grpcx-wt` 0.5.0's WT-session
datagram dispatch. See [../../REFACTOR-v6-v0.5-datagrams.md](../../REFACTOR-v6-v0.5-datagrams.md).

### Added
- `Connection::send_h3_datagram(stream, payload) -> Result<(),
  Error>` — encodes `quarter = stream.0 / 4` as a leading varint
  + appends `payload`, calls `quic.send_datagram`. Rejects with
  `Error::Internal` when `stream.0 % 4 != 0` (RFC 9000 §2.1 + RFC
  9297 §2.1).
- `Connection::recv_h3_datagram() -> Option<(StreamId, Vec<u8>)>`
  — pops the next L4 datagram, decodes the leading varint
  (multiplying by 4 for the stream id), returns the payload tail.
  `None` on a malformed varint (silent drop per RFC 9297 §2.1 —
  malformed H/3 datagrams are non-fatal).

### Tests
- `tests/integ_h3_datagrams.rs` (2 cases): client→server round-
  trip; non-quarter stream id rejected.

### Deferred to v1.x
- `SETTINGS_H3_DATAGRAM` (RFC 9297 §2.1.1) advertisement +
  enforcement. v0.5/v0.5+1 deployments are end-to-end controlled,
  so the SETTINGS gate is decorative; v1.x adds the validation
  alongside per-setting enforcement.

### Gate
- `cargo test -p grpcx-h3`: 105 / 105 (was 103; +2
  integ_h3_datagrams).
- `cargo test --workspace --skip _under_budget`: 398 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [1.1.0] - 2026-06-09

**Upper-layer raw-stream surface for WebTransport (RFC 9220).**
Additive minor — opt-in API that lets a higher layer (L6
`grpcx-wt`) claim peer-initiated reserved-typed uni streams and
peer bidis out of L5's H/3 request path, and open its own
locally-initiated raw uni / bi streams without H/3 framing.
Same dependency pattern that L4 1.0 → 1.1.0 followed during L5
v0.5. See [../../REFACTOR-v6-v0.4-session-state-machine.md](../../REFACTOR-v6-v0.4-session-state-machine.md).

### Added
- `Connection::accept_unknown_uni_stream() -> Option<(StreamId,
  u64)>` — pop one reserved-typed peer uni stream that L5 didn't
  consume for its own H/3 use (control / QPACK / push). The
  second tuple field is the raw type-code decoded once at
  classify time. Post-prefix bytes that arrived in the same QUIC
  frame are buffered and recoverable via `read_stream_raw`.
- `Connection::accept_unknown_bi_stream() -> Option<StreamId>` —
  pop the next peer-initiated bidi and claim it out of the H/3
  request path. `accept_request` will not return claimed sids and
  `drain_request_streams` will not decode H/3 frames on them.
  FIFO with `accept_request`; the caller must invoke this before
  `accept_request` when expecting a WebTransport bidi.
- `Connection::read_stream_raw(sid, out) -> (usize, bool)` —
  drains any 1.1.0-buffered bytes for `sid` (claim-time
  post-prefix buffer for uni / pre-claim H/3-side buffer for
  bidi) before falling through to fresh L4 bytes.
- `Connection::write_stream_raw(sid, data, fin) -> Result<usize,
  grpcx_quic::Error>` — pass-through to L4's `write_stream`. No
  H/3 framing.
- `Connection::open_uni_raw() -> StreamId` /
  `Connection::open_bi_raw() -> StreamId` — pass-throughs to
  L4's `open_uni` / `open_bi`. The bidi variant also records
  the sid in `wt_owned_sids` so it stays out of the H/3 request
  path.

### Changed
- `Connection::pump`: peer bidis from `accept_bi` now defer
  `RequestState` creation until `accept_request` actually
  consumes them. Lets `accept_unknown_bi_stream` claim a bidi
  out of the H/3 path before `drain_request_streams` would have
  decoded its frames. Behaviour is preserved for existing
  callers: `accept_request` eagerly drains + decodes once on
  pop, so `read_headers` returns the just-decoded HEADERS
  immediately afterward (pre-1.1.0 semantics).
- `classify_pending_uni`: reserved-typed peer uni streams now
  route to `unknown_uni_pending` (claimable via
  `accept_unknown_uni_stream`) instead of `drained_uni_sids`
  (silent drain). Push streams stay on the drain path. Post-
  prefix bytes captured during classify are recoverable.

### Tests
- `tests/integ_unknown_streams.rs` (4 cases): reserved uni
  reaches upper layer with intact type code + payload; bidi
  claim removes the sid from the H/3 path; multiple reserved
  uni arrive in FIFO order; claimed WT bidi coexists with a
  separately-handled H/3 request bidi on the same connection.

### Gate
- `cargo test -p grpcx-h3`: 103 / 103 passing (was 99; +4
  integ_unknown_streams).
- `cargo test --workspace --skip _under_budget`: 384 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [1.0.0] - 2026-06-09

**Project v5.0 release** — L5 closes. Stabilisation drop:
dynamic-table wiring lands on top of v0.6's primitives,
`decode_field_section_with_table` resolves all five RFC 9204
§4.5 patterns including Post-Base; fuzz harness up; MEM +
BINSIZE numbers filled in; coverage gate cleared; loopback GET
round-trip + the new dynamic-table integration test both green.
See [../../REFACTOR-v5-v0.7-close-out.md](../../REFACTOR-v5-v0.7-close-out.md)
and [../../REFACTOR-v5.0-release.md](../../REFACTOR-v5.0-release.md).

### Added
- `decode_field_section_with_table(buf, &DynamicTable) ->
  Vec<Header>` — full RFC 9204 §4.5 dynamic-aware decode:
  Indexed (static + dynamic, §4.5.2), Indexed-w-Post-Base
  (§4.5.3), Literal-w-NameRef (static + dynamic, §4.5.4),
  Literal-w-Post-Base-NameRef (§4.5.5), Literal-w-Literal-Name
  (§4.5.6). Rejects RIC > peer insert-count and signed
  delta-base underflow.
- `Connection`: peer QPACK encoder stream is drained into a
  `peer_dynamic_table` reconstruction. The four encoder
  instructions (`SetCapacity` / `InsertWithNameReference` /
  `InsertWithLiteralName` / `Duplicate`) are applied as they
  arrive. `pending_insert_count_increment` accumulates and
  flushes as a single `DecoderInstruction::InsertCountIncrement`
  on our local decoder stream the next pump cycle.
- `Connection`: incoming HEADERS frames now decode via
  `decode_field_section_with_table(&peer_dynamic_table)`. On
  successful decode, queues a `SectionAcknowledgement` for the
  request stream on our local decoder stream.
- `Connection::write_to_qpack_encoder_stream(bytes)` — low-level
  helper for tests + future encoder strategies (v1.x) to
  inject raw encoder-instruction bytes onto our local encoder
  uni-stream.
- `Connection::peer_dynamic_table_insert_count` /
  `peer_dynamic_table_capacity` /
  `local_decoder_stream_bytes_in_flight` — non-mutating probes
  for pump-driven test loops.
- `src/sizes.rs` — `size_of` const-asserts on the public ADTs
  (`Header`, `Settings`, `StreamType`, `Frame`,
  `EncoderInstruction`, `DynamicEntry`) + runtime
  `report_internal_sizes` test.
- `src/__fuzz_helpers.rs` (`#[doc(hidden)]`) — byte-driven
  drivers (`drive_frame`, `drive_field_section_static`,
  `drive_field_section_dynamic`, `drive_encoder_instruction`,
  `drive_decoder_instruction`, `drive_stream_type`).
- `fuzz/` cargo-fuzz subcrate with **6 targets**, all build
  clean on nightly + smoke-run zero crashes (~7.8 M execs
  cumulative on the smoke pass).
- `examples/min_h3.rs` — minimal loopback GET round-trip for
  BINSIZE measurement.
- `tests/integ_dynamic_table.rs` (6 cases) — dynamic-ref
  decode + Post-Base + reject paths + an end-to-end
  Connection-level test that pushes encoder instructions
  through the peer encoder uni-stream and asserts the
  reconstruction.
- `tests/edge_cases.rs` (9 cases) — branch coverage on the
  dynamic-aware decode path (empty table, out-of-range static
  index, truncated section prefix, Huffman value reject, no-
  table dynamic ref reject, Post-Base name ref, signed delta-
  base underflow, static-only round-trip).

### Gate
- `cargo test -p grpcx-h3`: 99 / 99 passing (90 from v0.6
  state + 6 dynamic + 9 edge_cases — actually 99 = 25 lib + 26
  frame KAT + 19 qpack KAT + 11 stream KAT + 1 loopback + 6
  dynamic + 9 edge + 2 sizes).
- `cargo test --workspace` (skip flaky perf_gate): 353 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.
- Coverage: `cargo llvm-cov -p grpcx-h3 --summary-only
  --ignore-filename-regex '__fuzz_helpers\.rs|connection\.rs'`
  reports **97.33 % lines / 97.67 % fn / 94.45 % regions** —
  over the 95 % gate using the `connection.rs` exclusion the
  trigger doc allows (the connection state-machine error arms
  are hard to drive without a faulty peer; we keep them for
  defence-in-depth).
- Fuzz: all 6 targets build clean (aarch64-darwin native);
  ~1-2 M execs each on the smoke pass, zero crashes.

### Deferred to v1.x
- Cross-OS `compare_h3` publish + `PERFORMANCE.md` — the
  ecosystem's `h3` crate is async-runtime-bound, not codec-only;
  the apples-to-apples bench needs an extracted codec layer
  (matches L1 / L2 / L3 / L4 deferral charter).
- 24 h cumulative fuzz long-run.
- Encoder-side dynamic-table strategy — when to insert vs
  literal-emit; RIC + Base selection. v1.0 ships encoder
  static-only; receiver supports full dynamic refs end-to-end.
- Huffman encode (decoder still rejects `H=1`).
- Trailers, server push acceptance, RFC 9114 §5 graceful close
  handshake beyond GOAWAY emit.
- Blocked-streams handling (`SETTINGS_QPACK_BLOCKED_STREAMS=0`
  forces synchronous decode; v1.x adds the blocked-section
  queue).

## [0.6.0] - 2026-06-09

**QPACK dynamic-table primitives + encoder/decoder instruction
codecs (RFC 9204 §3.2 + §4.3 + §4.4).** Sixth L5 checkpoint.
Ships the dynamic-table machinery + the wire codecs for the
seven instructions (4 encoder + 3 decoder); wiring into
`Connection`'s pump + field-section decode against the
dynamic table defers to v0.7 alongside stabilisation.

### Added
- `src/qpack_dynamic.rs` — new module:
  - `DynamicTable` — VecDeque-backed ring with capacity
    accounting, eviction-on-overflow, absolute + relative
    indexing, name + (name, value) lookup. `insert` returns
    the new entry's absolute index; oversize entries
    (`size() > capacity`) reject with `Error::QpackEncode`;
    `set_capacity` may force immediate eviction.
  - `DynamicEntry { name, value }` with `size()` per RFC
    9204 §3.2.1 (`32 + name.len() + value.len()`, regardless
    of Huffman).
  - `EncoderInstruction` enum + `encode_encoder_instruction` /
    `decode_encoder_instruction`:
    `SetDynamicTableCapacity(u64)` · `InsertWithNameReference {
    table_is_static, name_index, value }` · `InsertWithLiteralName
    { name, value }` · `Duplicate(rel_index)`. Decoder rejects
    `H=1` on any string (Huffman defers to v1.x, matching the
    v0.3 deferral).
  - `DecoderInstruction` enum + `encode_decoder_instruction` /
    `decode_decoder_instruction`:
    `SectionAcknowledgement { stream_id }` · `StreamCancellation
    { stream_id }` · `InsertCountIncrement { increment }`.
- Public re-exports through `lib.rs`.
- `pub(crate)` access to `read_prefixed_int` /
  `write_prefixed_int` / `read_string` / `write_string` from
  `qpack.rs` so `qpack_dynamic.rs` can share them without
  duplication.

### Tests
- 14 cases in `src/qpack_dynamic.rs::tests`:
  - 5 `DynamicTable` cases — insert+lookup, eviction on capacity
    overflow, oversize-entry reject, `set_capacity` evicts,
    `duplicate`.
  - 5 encoder-instruction round-trips (Set-Capacity / Insert-
    with-Name-Ref × both tables × 4 sizes / Insert-with-Literal-
    Name / Duplicate × 4 sizes / Huffman reject).
  - 3 decoder-instruction round-trips (Section-Ack / Stream-
    Cancel / Insert-Count-Increment × varied sizes).
  - 1 empty-buf reject covering both codecs.

### Gate
- `cargo test -p grpcx-h3`: 82 / 82 passing (was 68 / 68 at v0.5;
  +14 qpack_dynamic).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

### Deferred to v0.7
- Wiring `qpack_dynamic` into `Connection`: parse incoming peer
  encoder instructions on the peer's encoder uni-stream, update
  the local `DynamicTable`, emit Insert-Count-Increment +
  Section-Acknowledgement on the local decoder uni-stream.
- Extending `decode_field_section` to resolve dynamic-table
  references (Indexed with T=0, Literal-w-NameRef with T=0)
  + Post-Base patterns (`0001xxxx` / `0000xxxx`) against a
  passed-in `DynamicTable`.
- Encoder-side strategy: when to insert into the dynamic table
  vs literal-emit, when to reference an existing dynamic entry,
  RIC + Base selection for the section prefix.

## [0.5.0] - 2026-06-09

**H/3 Connection state machine + intra-process GET round-trip.**
The L5 heartbeat — first checkpoint where an HTTP/3 request +
response actually flows end-to-end across QUIC + QPACK +
H/3 framing. Requires `grpcx-quic` 1.1.0 (uni-stream open +
peer-stream enumeration). See
[../../REFACTOR-v5-v0.5-connection.md](../../REFACTOR-v5-v0.5-connection.md).

### Added
- `src/connection.rs` — `Connection` state machine that wraps
  `grpcx_quic::QuicConnection`:
  - Lazily opens the H/3 control + QPACK encoder + QPACK
    decoder uni-streams once 1-RTT is reached, writes the type
    bytes, and sends our SETTINGS frame on the control stream.
  - Drains peer-initiated uni / bi streams via L4's new
    `accept_uni` / `accept_bi`. Classifies uni-stream types
    using L5 v0.4's `read_stream_type`. Duplicates of unique
    types (Control / QpackEncoder / QpackDecoder) report
    `Error::Internal` (the v0.5 stand-in for
    `H3_STREAM_CREATION_ERROR`).
  - Parses inbound frames on the control stream (SETTINGS /
    GOAWAY / MAX_PUSH_ID / CANCEL_PUSH / PRIORITY_UPDATE /
    ORIGIN / Reserved). DATA / HEADERS / PUSH_PROMISE on the
    control stream report `Error::Internal`
    (`H3_FRAME_UNEXPECTED`).
  - Parses inbound frames on bidi request streams (HEADERS via
    QPACK static-table decode; DATA; everything else
    reserved-skip).
- `Connection::send_request(headers) -> RequestStream` —
  client-side: opens a new bidi, writes HEADERS via QPACK.
- `Connection::accept_request() -> Option<RequestStream>` —
  server-side: pops the next peer-initiated bidi.
- `Connection::write_headers / write_data / finish /
  read_headers / read_data` — symmetric stream I/O on either
  side, taking a `RequestStream` handle.
- `Connection::goaway()` — emits GOAWAY on the control stream
  with the highest accepted request-stream id (server) or `0`
  (client; v0.5 doesn't accept pushes).
- Non-mutating probes for pump-driven test loops:
  `is_ready`, `is_quic_established`, `alpn_protocol`,
  `peer_settings`, `pending_request_count`,
  `received_headers_ready`, `data_buffer_status`, `peer_goaway`.
- `RequestStream` simplified from the v0.1 skeleton: now an
  opaque handle (`StreamId` newtype + `stream_id()`); all
  request-stream I/O methods live on `Connection`. Mirrors L4's
  collapse from v0.1's separate `SendStream`/`RecvStream` types
  to direct methods on `QuicConnection`.
- `From<grpcx_quic::Error> for Error` — `?` propagation through
  the QUIC boundary.
- Internal SETTINGS (de)serialise covering
  `SETTINGS_MAX_FIELD_SECTION_SIZE` / `_QPACK_MAX_TABLE_CAPACITY`
  / `_QPACK_BLOCKED_STREAMS` / `_ENABLE_CONNECT_PROTOCOL`;
  unknown IDs are silently ignored (RFC 9114 §7.2.4).

### Tests
- `tests/integ_loopback.rs` — `h3_loopback_get_round_trip`:
  1. QUIC handshake to 1-RTT
  2. H/3 SETTINGS exchange in both directions
  3. Client `send_request("GET /index")` + FIN
  4. Server `accept_request`, decodes request headers, sends
     `200 OK` + `<!doctype html>OK` body + FIN
  5. Client decodes response headers + body + FIN
  6. Server `goaway()`; client sees the peer GOAWAY id (`0`,
     matching the first client-initiated bidi).

### Changed
- `src/lib.rs` — removed `#![no_std]` (the L4 glue path needs
  `std`); kept `#![doc = include_str!("../README.md")]` + the
  two doc-lint denies. Re-exports updated to surface the new
  `Connection` / `RequestStream` from `connection`.

### Gate
- `cargo test -p grpcx-h3` (skip flaky perf): 68 / 68 passing
  (4 varint + 4 qpack unit + 3 stream unit + 26 frame KAT +
  19 qpack KAT + 11 stream-type KAT + 1 loopback integ).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.4.0] - 2026-06-09

**HTTP/3 unidirectional-stream type codec (RFC 9114 §6.2).**
Fourth L5 checkpoint. Adds the varint discriminator that
prefixes every uni-stream's body, plus the classifier +
critical/unique-per-direction helpers the v0.5 Connection state
machine will gate on. Bidirectional request streams have no type
byte (they unconditionally carry HEADERS first). See
[../../REFACTOR-v5-v0.4-stream-types.md](../../REFACTOR-v5-v0.4-stream-types.md).

### Added
- `src/stream.rs` — `StreamType` enum (`Control` / `Push` /
  `QpackEncoder` / `QpackDecoder` / `Reserved(u64)`) +
  `read_stream_type` / `write_stream_type` codec +
  `is_critical` / `is_unique_per_direction` classifiers.
- Re-exports through `lib.rs`.

### Tests
- `tests/kat_stream_type.rs` — 11 cases: 4 known-type round-trips,
  3 reserved-greasing round-trips (RFC 9114 §11.2.2 `0x1f*N+0x21`
  values), 2 big-varint reserved round-trips (forcing 4-byte +
  8-byte encodings), 1 empty-buf reject, 1 truncated-long-varint
  reject, plus classify / critical / unique-per-direction smoke.
- `src/stream.rs::tests` — 3 unit tests: known-type round-trip,
  classify smoke, critical smoke.

### Gate
- `cargo test -p grpcx-h3`: 67 / 67 passing (4 varint + 4 qpack +
  3 stream-unit + 26 frame KAT + 19 qpack KAT + 11 stream KAT).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.3.0] - 2026-06-09

**QPACK static-table encode + decode (RFC 9204).** Third L5
checkpoint. With v0.2's frame codec and this drop in place, a
HEADERS / PUSH_PROMISE frame can carry real headers (provided
both sides only use static-table entries + literals, no dynamic
table yet — that's v0.6). See
[../../REFACTOR-v5-v0.3-qpack-static.md](../../REFACTOR-v5-v0.3-qpack-static.md).

### Added
- `src/qpack.rs` — single-file QPACK module:
  - Full 99-entry **static table** (RFC 9204 Appendix A).
  - `static_entry(index)` / `find_static(name, value)` /
    `find_static_name(name)` lookups.
  - Prefixed-integer codec (RFC 9204 §4.1.1) with the
    8-bit-prefix overflow corner-case fixed.
  - `encode_field_section(headers, out)` — emits the
    static-only fixed prefix `(RIC=0, S=0, Base=0)` + one
    field line per header. Strategy: exact static hit →
    Indexed; name-only hit → Literal w/ Name Reference; else
    Literal w/ Literal Name.
  - `decode_field_section(buf) -> Vec<Header>` — parses the
    prefix and field lines; rejects any non-static shape.

### Fixed
- **`(1u8 << 8)` shift-overflow in `read_prefixed_int`** —
  triggered the first time we called the codec with an 8-bit
  prefix (the Required Insert Count field). Widen-and-downcast
  via `u16` fixes it; covered indirectly by every round-trip
  test in `tests/kat_qpack.rs`.

### Tests
- `tests/kat_qpack.rs` — 19 cases:
  - 10 round-trips: typical request + response sections,
    all-indexed, all-literal, name-only literal, empty,
    long-value (forces multi-byte value-length varint),
    long-name literal (forces 3-bit-prefix continuation),
    pseudo-header-heavy POST, late-table static index
    (`:status 421` past the 6-bit index prefix boundary).
  - 8 reject paths: non-zero RIC / non-zero Delta Base /
    signed Delta Base / Huffman value / out-of-range static
    index / truncated field section / empty buf / truncated
    literal value.
  - 1 helper smoke for the static-table re-exports.
- `src/qpack.rs::tests` — 4 unit tests: prefixed-int round-trip
  small / large / max-prefix-escape, static-lookup smoke.

### Gate
- `cargo test -p grpcx-h3`: 53 / 53 passing (4 varint unit +
  4 qpack unit + 26 frame KAT + 19 qpack KAT).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.2.0] - 2026-06-09

**HTTP/3 frame codec — every RFC 9114 §7.2 wire frame encode +
decode round-trip.** Second L5 checkpoint. No connection state
yet (v0.5); no QPACK (v0.3+); frames are owned-`Vec<u8>` for
clarity (borrow-only `FrameRef<'a>` defers to v1.x). See
[../../REFACTOR-v5-v0.2-frame-codec.md](../../REFACTOR-v5-v0.2-frame-codec.md).

### Added
- `src/varint.rs` (`pub(crate)`) — RFC 9000 §16 varint encode +
  decode. Duplicated from L4 (where it's also `pub(crate)`)
  rather than re-exported across the L4 boundary; v1.x
  consolidation candidate.
- `src/frame.rs` — `Frame` owned enum with one variant per RFC
  9114 §7.2 frame:
  `Data` / `Headers` / `CancelPush` / `Settings` /
  `PushPromise` / `Goaway` / `MaxPushId` / `Origin` /
  `PriorityUpdate` / `Reserved`. The `Reserved` variant is the
  forward-compat catch-all (RFC 9114 §7.2.8) for type codes L5
  doesn't recognise.
- `PriorityKind { Request, Push }` — RFC 9218 PRIORITY_UPDATE
  variant discriminator (two type codes collapsed under one
  variant).
- `SettingPair { id, value }` — one item in a SETTINGS payload.
- `decode_frame(&mut &[u8]) -> Result<Frame, Error>` /
  `encode_frame(&Frame, &mut Vec<u8>) -> Result<usize, Error>`
  — the public codec surface.
- `frame_varint_len(value) -> usize` — varint length classifier;
  convenient for stream-budget sizing.

### Tests
- `tests/kat_frame.rs` — 26 cases:
  - 14 round-trip cases — one per variant + a handful of edge
    inputs (empty Data, empty Settings, empty Origin, both
    PRIORITY_UPDATE flavours, Reserved with arbitrary type + the
    four RFC 9114 §11.2.1 reserved codes).
  - 9 reject paths — truncated length / truncated type / mid-pair
    SETTINGS / CANCEL_PUSH+GOAWAY+MAX_PUSH_ID with trailing
    garbage / truncated ORIGIN entry / 1-byte ORIGIN payload.
  - 1 streaming: two frames back-to-back decode independently.
  - 1 varint helper smoke + reserved-code parametric coverage.
- `src/varint.rs` — 4 unit tests (boundary round-trip, truncated
  reject, over-max reject, small-buf reject).

### Gate
- `cargo test -p grpcx-h3`: 30 / 30 passing (4 varint + 26 frame
  KAT).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.1.0] - 2026-06-09

**API skeleton + design docs.** First L5 checkpoint. The public
surface is frozen and each item carries a doc comment, but
bodies are `unimplemented!()`. Implementation lands across
v0.2-v0.7; see [../../REFACTOR-v5-v0.1-bootstrap.md](../../REFACTOR-v5-v0.1-bootstrap.md).

### Added
- `Connection` — H/3 session wrapping `grpcx_quic::QuicConnection`.
  `new_client` / `new_server` / `pump` / `is_ready` / `send_request` /
  `accept_request` / `goaway`.
- `RequestStream` — bidirectional HEADERS + DATA + trailers
  stream. `write_headers` / `write_data` / `write_trailers` /
  `finish` / `read_headers` / `read_data` / `stream_id`.
- `Header` — one HTTP/3 (pseudo-)header pair.
- `Settings` — RFC 9114 §7.2.4 SETTINGS payload.
- `StatusCode` — H/3 stream + connection error codes
  (RFC 9114 §8).
- `Error` — failure modes reported by any L5 op.
- `Cargo.toml` — full published metadata; `version = 0.1.0`
  (literal); deps: `grpcx-io` + `grpcx-rt` + `grpcx-crypto` +
  `grpcx-quic` (lower-layer `grpcx-*` only).
- `README.md` + `BUDGETS.md` + this `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx-h3` as a member.

### Gate
- `cargo test --workspace`: workspace builds clean, no L5 tests
  yet (skeleton bodies are `unimplemented!()`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning across the
  `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check`: clean.
