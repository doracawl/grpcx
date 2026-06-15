# Changelog

All notable changes to `grpcx-ws` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.1.0] below)

## [1.1.0] - 2026-06-10

**Stones consume — `src/sha1.rs` + `src/base64.rs` + the
internal `Cursor` + the internal `is_token_char` lift out
to in-house `stones` workspace crates**. Zero behavioural
change; full test suite (121 unit + KAT tests) stays
green. See `../../../stones/` and the `In-house stones`
section of `../../DEPS_AUDIT.md`.

### Added
- `[dependencies]` block now lists four cross-workspace
  path deps to `../../../stones/crates/*`, each pinned to
  the stone's published version:
  - `base64-rfc4648 = "0.1.0"`
  - `byte-cursor = "0.1.1"`
  - `http1-syntax = "0.1.0"`
  - `sha1-rfc3174 = "0.1.0"`
- Stones are 0-runtime-dep, pure-Rust, `no_std + alloc`
  crates; the transitive dep count of `grpcx-ws` stays
  zero.

### Removed
- `src/sha1.rs` (~90 LoC) — lifted verbatim into
  `sha1-rfc3174` stone.
- `src/base64.rs` (~50 LoC) — lifted verbatim into
  `base64-rfc4648` stone.
- The private `Cursor` struct in `src/extensions.rs`
  (~80 LoC) — replaced by `byte_cursor::Cursor` (which
  came from the second copy of the same shape in
  `grpcx-pb/src/lexer.rs`).
- The private `is_token_char(c: char)` / `read_token` /
  `read_quoted` helpers in `src/extensions.rs` (~50 LoC)
  — replaced by `http1_syntax::is_token_char` /
  `read_token` / `read_quoted_string`.

### Migration

API surface is unchanged. The crate's public ADTs and
entry-point functions keep their v1.0 signatures + return
shapes. Downstream callers should see no diff beyond
needing `cargo update` to pick up the new resolved tree.

## [1.0.0] - 2026-06-10

**Project v9.0 release** — L9 closes. v0.7 stabilisation
pass: `sizes.rs` + `examples/min_ws.rs` + `BUDGETS.md`
filled with measured numbers. See
[../../REFACTOR-v9-v0.7-close-out.md](../../REFACTOR-v9-v0.7-close-out.md)
and [../../REFACTOR-v9.0-release.md](../../REFACTOR-v9.0-release.md).

### Added
- `src/sizes.rs` (~125 LoC):
  - 11 `const _: () = assert!(…)` compile-time size bounds
    on every public ADT (`Opcode` / `MessageKind` /
    `CloseCode` / `WsError` / `Frame` / `Message` /
    `Handshake` / `ReassemblyOutput` / `Reassembler` /
    `ExtensionOffer` / `PermessageDeflateOffer`).
  - `report_internal_sizes` test (cfg-gated `extern crate
    std;`) printing the MEM table for `BUDGETS.md`
    copy-paste.
- `examples/min_ws.rs` (~110 LoC) — 6-step E2E:
  - Client handshake build (RFC § 1.3 vector input).
  - Server parse + `derive_accept_key` → matches the
    canonical accept key.
  - Server response build.
  - Frame encode with masking → matches RFC § 5.7 byte
    pattern.
  - Frame decode → unmasked payload preserved.
  - Reassembler round-trip on 3-frame fragmented Text.
  - `permessage-deflate` extension parse + typed
    PMD struct.
  - `WsError::FragmentedControlFrame` smoke on a Ping
    with `FIN=0`.

### Filled
- `BUDGETS.md`:
  - MEM column: 11 rows of measured `size_of`, all under
    bounds. Tightest packings: `Opcode` and `MessageKind`
    at 1 byte each.
  - BINSIZE: 227.2 KiB total `.text`; `grpcx_ws`
    contribution 7.0 KiB (3.1 %). Between L7's 424 B /
    0.2 % (pure codec) and L8's 27.1 KiB / 10.8 %
    (parser-heavy).

### Changed
- `src/lib.rs`: `mod sizes;` declared.

### Out of scope at v1.0 (v9.1 / v1.x)
- Fuzz harness (`__fuzz_helpers.rs` + `fuzz/` subcrate +
  smoke run).
- `compare_tungstenite` head-to-head harness +
  `tungstenite` workspace dev-dep + `DEPS_AUDIT.md`
  entry.
- `cargo llvm-cov` coverage measurement (≥ 95 %).
- Autobahn TestSuite integration.
- `permessage-deflate` real compression (negotiation
  ships at v0.6; DEFLATE state machine v1.x).
- WebSocket-over-HTTP/2 (RFC 8441) — v2.x cross-layer.
- Random-nonce helper (currently caller-supplied).
- base64 decode (currently encode-only).
- Directionality enforcement (currently codec is
  symmetric; `WsError::MaskingMismatch` reserved for
  v1.x).
- Post-RFC IANA close codes (1012-1014) — strict RFC
  6455 today.
- Message-length cap on `Reassembler` — adapter pattern;
  v1.x.
- L3 layer-up patch for SHA-1 + base64 — lift from
  L9-inlined `pub(crate)` helpers to public
  `grpcx-crypto` surfaces.

### Gate
- `cargo test -p grpcx-ws`: 128 / 128 passing.
- `cargo test --workspace --skip _under_budget`: **775 / 0**
  (was 774 at v0.6 close; +1 from `sizes::tests::report_internal_sizes`).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.
- `cargo run --example min_ws --release`: exits 0
  (6-step assertion block passes).

## [0.6.0] - 2026-06-10

**Extension negotiation surface.** Sixth L9 checkpoint.
Quote-aware RFC 6455 § 9.1 `Sec-WebSocket-Extensions`
parser + formatter; strongly-typed `PermessageDeflateOffer`
for the RFC 7692 extension. Negotiation only — real
compression remains v1.x. See
[../../REFACTOR-v9-v0.6-extensions.md](../../REFACTOR-v9-v0.6-extensions.md).

### Added
- `src/extensions.rs` (~370 LoC incl. 7 unit tests).
- `ExtensionOffer { name, params: Vec<(String,
  Option<String>)> }` — structured offer.
- `pub fn parse_extension_header(value: &str) ->
  Result<Vec<ExtensionOffer>, WsError>` — quote-aware
  comma-separated list parser.
- `pub fn parse_extension_offer(value: &str) ->
  Result<ExtensionOffer, WsError>` — single-offer
  variant (rejects extra trailing characters).
- `pub fn format_extension_header(offers: &[ExtensionOffer])
  -> String` — symmetric inverse; auto-quotes values
  containing non-token characters.
- `pub fn find_offer_by_name(offers, name) ->
  Option<&ExtensionOffer>` — case-sensitive lookup.
- `PermessageDeflateOffer { server_no_context_takeover,
  client_no_context_takeover, server_max_window_bits:
  Option<u8>, client_max_window_bits: Option<u8> }` +
  `parse(&ExtensionOffer) -> Result<Self, WsError>` +
  `to_offer(&self) -> ExtensionOffer`.
- `tests/kat_extensions.rs` (23 cases):
  - 7 accept cases — single offer (no params / bare-key /
    key=value / quoted value / escape in quoted) +
    multiple offers + whitespace tolerance.
  - 2 round-trip cases.
  - 5 `PermessageDeflateOffer` cases (no params / all
    params / to_offer round-trip / window-bits boundary
    + range reject / unknown param reject / wrong name
    reject).
  - 2 `find_offer_by_name` cases.
  - 2 `parse_extension_offer` cases.
  - 5 reject paths (trailing comma / unterminated quote
    / non-ASCII in token / empty name / empty value).
  - 1 empty header → empty list smoke.

### Changed — `src/lib.rs`
- `mod extensions;` added.
- 6 pub re-exports: `ExtensionOffer`,
  `PermessageDeflateOffer`,
  `parse_extension_header`, `parse_extension_offer`,
  `format_extension_header`, `find_offer_by_name`.

### Convention
- Extension tokens are case-sensitive (RFC 6455 § 9.1).
- Parameter values without parameters (bare keys) carry
  `None`; with `=` carry `Some(value)`.
- Token character set is RFC 7230 § 3.2.6; non-token
  characters in a value force the formatter to quote.
- `permessage-deflate` window-bits restricted to 8..=15
  per RFC 7692 § 7.1.2.2.
- `Handshake.extensions: Vec<String>` is unchanged at v0.6
  — the structured surface is opt-in via the new parse
  functions.

### Out of scope (v1.x)
- Real DEFLATE compression + sliding-window state
  machine.
- RSV1 reservation enforcement for compressed frames.
- Multi-extension parameter compatibility resolution at
  the session layer.

### Gate
- `cargo test -p grpcx-ws`: 127 / 127 passing (was 97 at
  v0.5 close; +30 = 7 extensions unit + 23
  kat_extensions).
- `cargo test --workspace --skip _under_budget`: **774 / 0**
  (was 744 at v0.5 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

## [0.5.0] - 2026-06-10

**HTTP/1.1 upgrade handshake.** Fifth L9 checkpoint. The
four v0.1 handshake entry-point stubs become real builders
and parsers; the RFC 6455 § 1.3 canonical test vector is
byte-locked. SHA-1 and base64 land as inlined `pub(crate)`
helpers (liftable to L3 in a future layer-up patch). See
[../../REFACTOR-v9-v0.5-handshake.md](../../REFACTOR-v9-v0.5-handshake.md).

### Added
- `src/sha1.rs` (~140 LoC incl. 4 unit tests) — RFC 3174
  reference implementation; covers empty input, "abc" (RFC
  3174 § 7.3 vector), "The quick brown fox …", and the
  WebSocket § 1.3 intermediate-hash vector.
- `src/base64.rs` (~70 LoC incl. 3 unit tests) — RFC 4648
  standard-alphabet encode with `=` padding; covers
  empty + 1/2/3/4/5/6-byte boundaries + the WebSocket
  § 1.3 accept-key vector.
- `src/handshake.rs` (~310 LoC incl. 4 unit tests) — key
  derivation + verification + builders + parsers.
- `pub fn derive_accept_key(request_key: &str) -> String`
  — RFC 6455 § 4.2.2 step 5:
  `base64(SHA-1(request_key + WS_MAGIC_GUID))`.
- `pub fn verify_accept_key(request_key, accept_key)
  -> Result<(), WsError>` — client-side helper.
- `tests/kat_handshake.rs` (19 cases) — **RFC § 1.3
  reference vector lock** (the canonical
  `dGhlIHNhbXBsZSBub25jZQ==` → `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`
  test), client + server build cases, round-trip via
  parser, `verify_accept_key` mismatch reject, 4 client
  reject paths + 3 server reject paths, structural rejects
  (no separator, malformed start line), case-insensitive
  header acceptance, trailing-bytes ignored.

### Changed — `src/lib.rs`
- `mod base64;` + `mod handshake;` + `mod sha1;` added.
- `pub use handshake::{derive_accept_key, verify_accept_key};`
- All 4 v0.1 handshake entry-point stubs replaced with
  thin re-exports to `handshake::*`.
- **Signature change** (pre-1.0 break):
  `perform_client_handshake_request` now takes
  `(host: &str, path: &str, handshake: &Handshake)`. v0.1
  had `(host: &str, path: &str)`. The new
  `handshake` parameter lets the caller supply the
  `Sec-WebSocket-Key` nonce + offered subprotocols +
  extensions. Pre-1.0 evolution; locked at v0.5 with the
  REFACTOR + this CHANGELOG entry.

### Convention
- L9 stays self-contained: SHA-1 + base64 live as
  `pub(crate)` helpers inside L9 rather than as an L3
  surface bump. Future layer-up patch can lift them
  (matches the L4→L6 pattern from prior layers).
- HTTP/1.1 parsing is inline; no `httparse` or other
  external dep. Headers compared case-insensitively;
  `Connection:` value treated as a comma-separated token
  list with case-insensitive `Upgrade` token check.
- Caller supplies the random nonce — no L9-level
  randomness source. v0.7+ may add a feature-flagged
  convenience wrapper.

### Out of scope (per the v0.x partition)
- Extension negotiation surface (parameter parsing,
  offer/accept handshake) — v0.6.
- Stabilisation + Autobahn subset — v0.7.
- Random-nonce helper — v0.7 or v1.x.
- base64 decode — v0.6+ if needed (handshake only needs
  encode).

### Gate
- `cargo test -p grpcx-ws`: 97 / 97 passing (was 67 at
  v0.4 close; +30 = 4 sha1 unit + 3 base64 unit + 4
  handshake unit + 19 kat_handshake).
- `cargo test --workspace --skip _under_budget`: **744 / 0**
  (was 714 at v0.4 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean (one `int_plus_one` lint fixed during close).
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

## [0.4.0] - 2026-06-10

**Message reassembly + UTF-8 validation.** Fourth L9
checkpoint. `Reassembler` state machine turns a stream of
`Frame`s (data + interleaved control) into reassembled
`Message`s per RFC 6455 § 5.4; emitted Text payloads are
UTF-8-validated per § 5.6. See
[../../REFACTOR-v9-v0.4-reassembly.md](../../REFACTOR-v9-v0.4-reassembly.md).

### Added
- `Reassembler` struct + `Reassembler::new()` /
  `Default` impl + `pub fn feed(&mut self, frame: Frame)
  -> Result<ReassemblyOutput, WsError>`.
- `ReassemblyOutput` enum: `Pending` /
  `Message(Message)` / `Control(Frame)`.
- `WsError::ProtocolError(&'static str)` — generic
  protocol-violation bucket used by the reassembler.
  Additive change to the v0.1 frozen ADT, locked at v0.4
  (pre-1.0).
- `src/reassembler.rs` (~200 LoC incl. 5 unit tests).
- `tests/kat_reassembly.rs` (16 cases):
  - 2 single-frame complete (Text + Binary).
  - 4 fragmentation cases (2-frame Text, 3-frame Text,
    3-frame Binary, empty-payload fragmentation).
  - 2 control-frame pass-through (Ping mid-fragmentation
    preserves state; Close at empty state).
  - 3 protocol-error paths (continuation at empty, new
    Text mid-fragmentation, new Binary mid-fragmentation).
  - 3 UTF-8 validation cases — including the spec-
    sensitive `fragment_boundary_splits_multibyte_utf8`
    test that proves whole-message validation accepts
    a 🦀 codepoint split across a fragment boundary.
  - 1 `Default::default()` accepts first frame smoke.
  - 1 sequential-messages-on-same-reassembler smoke
    (state resets to Empty after each message).

### Changed
- `src/lib.rs`:
  - `mod reassembler;` added.
  - `pub use reassembler::{Reassembler, ReassemblyOutput};`.
- `WsError` rustdoc — added `ProtocolError` arm
  description.

### Convention
- Control frames (Close / Ping / Pong) pass through
  `feed()` returning `ReassemblyOutput::Control(frame)`
  without touching the fragmentation state. RFC 6455
  § 5.5 allows this; the v0.4 reassembler matches.
- UTF-8 validation runs on emit (after reassembly),
  never per-fragment. A multi-byte codepoint can
  legally straddle a fragment boundary.

### Out of scope (deferred per the v0.x partition)
- HTTP/1.1 upgrade handshake — v0.5.
- Extension negotiation surface — v0.6.
- Message-length cap (hostile peer could OOM) — v1.x
  adapter pattern; v0.4 trusts the byte stream.

### Gate
- `cargo test -p grpcx-ws`: 67 / 67 passing (was 46 at
  v0.3 close; +21 = 5 reassembler unit + 16 kat_reassembly).
- `cargo test --workspace --skip _under_budget`: **714 / 0**
  (was 693 at v0.3 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

## [0.3.0] - 2026-06-10

**Control frames + close codes.** Third L9 checkpoint.
The frame decoder now enforces RFC 6455 § 5.5 control-
frame rules (FIN=1 + payload ≤ 125 bytes), and the
`CloseCode` ADT gains a lookup table + close-payload
codec matching RFC 6455 § 7. See
[../../REFACTOR-v9-v0.3-control-frames.md](../../REFACTOR-v9-v0.3-control-frames.md).

### Added
- `Opcode::is_control(&self) -> bool` — `true` for Close /
  Ping / Pong; `false` for Continuation / Text / Binary.
- `WsError::FragmentedControlFrame` — new variant for
  control opcodes with `FIN=0`. Additive change to the
  v0.1 frozen ADT, locked at v0.3 (pre-1.0).
- `CloseCode::from_u16(u16) -> Result<Self, WsError>` —
  RFC 6455 § 7.4 lookup. Standard codes 1000-1011 (minus
  1004/1005/1006) map to named variants; 3000-4999 →
  `Custom(u16)`. All other values (incl. 1012-1014, 1015,
  out-of-band) → `WsError::InvalidCloseCode(u16)`.
- `CloseCode::to_u16(&self) -> u16` — symmetric inverse;
  always produces a valid wire code.
- `CloseCode::is_reserved(code: u16) -> bool` —
  convenience predicate matching `from_u16(code).is_err()`.
- `src/close.rs` (new, ~175 LoC incl. 5 unit tests):
  - `CloseCode` `impl` block with the three helpers
    above.
  - `pub fn encode_close_payload(code: Option<CloseCode>,
    reason: &str) -> Vec<u8>` — RFC 6455 § 5.5.1: empty
    when `code = None`, else 2-byte BE code + UTF-8
    reason.
  - `pub fn decode_close_payload(&[u8]) -> Result<(Option<CloseCode>, String), WsError>`
    — inverse; empty payload → `(None, "")`; 1-byte →
    `Truncated`; otherwise parse code via `from_u16` then
    UTF-8-validate the reason.
- `tests/kat_close.rs` (15 cases):
  - 6 close-payload round-trip cases (empty / code-only /
    ASCII reason / Unicode reason / `Custom(4042)` byte
    pattern / `code=None` drops reason).
  - 3 close-payload reject paths (1-byte truncated /
    reserved code 1004 / non-UTF-8 reason).
  - 3 control-frame reject paths (Ping FIN=0 / Close
    FIN=0 / Ping 126-byte payload via extended length).
  - 1 control-frame boundary accept (Ping with 125-byte
    payload short-form).
  - 1 data-frame fragmentation still accepts (FIN=0 +
    Text — the v0.3 rule only applies to control
    opcodes).
  - 1 `Opcode::is_control` classification smoke.

### Changed
- `src/codec.rs::decode` — after opcode classification +
  length resolution, if `opcode.is_control()`:
  - Reject if `!fin` → `WsError::FragmentedControlFrame`.
  - Reject if `length > 125` → `WsError::PayloadTooLarge`.
  The encoder is unchanged (trusts caller-provided
  `Frame`).
- `src/lib.rs`:
  - `mod close;` added.
  - `pub use close::{decode_close_payload, encode_close_payload};`
- Doc updates on `WsError`: added the
  `FragmentedControlFrame` rustdoc.

### Convention
- The encoder is "trust the input": it doesn't validate
  the caller's `Frame` against control-frame rules. The
  decoder enforces.
- `code = None` + non-empty `reason` produces an empty
  payload; the reason is silently dropped because the
  wire has no slot for it without a code. Documented on
  `encode_close_payload`.

### Out of scope (per the v0.x partition)
- Text-payload UTF-8 validation — v0.4.
- Message reassembly across fragmented frames — v0.4.
- HTTP/1.1 upgrade handshake — v0.5.
- Directionality enforcement (client must mask, server
  must not) — session layer; v1.x.

### Gate
- `cargo test -p grpcx-ws`: 46 / 46 passing (was 26 at
  v0.2 close; +20 = 5 close unit + 15 kat_close).
- `cargo test --workspace --skip _under_budget`: **693 / 0**
  (was 673 at v0.2 close).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean (one `doc_lazy_continuation` + one `useless_vec`
  lint fixed during close).
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

## [0.2.0] - 2026-06-10

**Frame codec — RFC 6455 § 5.2.** Second L9 checkpoint.
`encode_frame` / `decode_frame` are now functional;
callers can round-trip any single WebSocket frame through
the public entry points. See
[../../REFACTOR-v9-v0.2-frame-codec.md](../../REFACTOR-v9-v0.2-frame-codec.md).

### Added — `src/codec.rs` (~230 LoC incl. 3 unit tests)
- `pub(crate) fn encode(&Frame, &mut Vec<u8>)` — packs
  byte 0 (`FIN` + `RSV1/2/3` + opcode) + byte 1 (`MASK` +
  7-bit length field), emits the appropriate extended-
  length form, appends the 4-byte key when masked, then
  appends the payload (XOR-masked if `Frame.mask.is_some()`).
- `pub(crate) fn decode(&[u8]) -> Result<(Frame, usize), WsError>`
  — symmetric inverse with smallest-form enforcement on
  extended lengths (126 must not encode ≤ 125; 127 must
  not encode ≤ 0xFFFF) and 64-bit MSB rejection.
- `opcode_to_u8` / `opcode_from_u8` — pack/unpack tables
  for the 6 standard opcodes; reserved values (0x3-0x7 /
  0xB-0xF) surface as `WsError::InvalidOpcode(u8)`.
- `apply_mask` — 4-byte XOR loop; pure, no allocation.

### Changed — `src/lib.rs`
- `mod codec;` added.
- `encode_frame` / `decode_frame` bodies rewired to call
  `codec::encode` / `codec::decode`. v0.1's
  `unimplemented!()` sentinels removed.

### Tests
- `src/codec.rs::tests` (3 cases) — opcode round-trip,
  reserved-opcode rejection (10 values), mask XOR round-
  trip.
- `tests/kat_frame.rs` (21 cases) — public-API frame
  round-trip:
  - 4 opcode round-trip cases covering all 6 standard
    opcodes (Continuation / Text / Binary / Close+Ping+Pong).
  - 5 length-encoding boundary cases (0 / 125 / 126 /
    0xFFFF / 0x10000) with byte-pattern locks.
  - 2 masking cases including the RFC 6455 § 5.7 reference
    vector (masked "Hello" → `7F 9F 4D 51 58`).
  - 9 reject paths: reserved opcode / RSV bit set /
    overlong length-126 / overlong length-127 / 64-bit MSB
    set / truncated header / truncated extended length /
    truncated mask key / truncated payload.
  - 1 trailing-bytes case (decode returns consumed-count
    so the caller can re-enter with the remainder).

### Convention
- `Frame.payload` is **always** the unmasked form. Both
  encode and decode preserve this — encode applies the
  XOR mask while writing the wire, decode undoes it after
  reading. A round-trip with the same `Frame` reproduces
  the original wire bytes byte-for-byte.

### Out of scope (deferred per the v0.x partition)
- Control-frame constraints (FIN=1, payload ≤ 125 bytes)
  — v0.3.
- `CloseCode::from_u16` + reserved code rejection — v0.3.
- UTF-8 validation on Text payloads — v0.4.
- Message reassembly — v0.4.
- RSV-bit allowance under extension negotiation — v0.6.
- Directionality enforcement (client must mask, server
  must not) — session-layer concern; v1.x.

### Gate
- `cargo test -p grpcx-ws`: 26 / 26 passing (was 2 at
  v0.1 close; +24 = 3 codec unit + 21 kat_frame).
- `cargo test --workspace --skip _under_budget`: **673 / 0**
  (was 649 at v0.1 close; +24).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check --all`: clean.

## [0.1.0] - 2026-06-10

**API skeleton + design docs.** First L9 checkpoint. The
public surface is frozen and each item carries a doc
comment, but bodies are `unimplemented!()`. Implementation
lands across v0.2-v0.7; see
[../../REFACTOR-v9-v0.1-bootstrap.md](../../REFACTOR-v9-v0.1-bootstrap.md).

### Added
- `Frame` — `(fin, rsv1, rsv2, rsv3, opcode, mask,
  payload)` shaped per RFC 6455 § 5.2. Masking modelled as
  `Option<[u8; 4]>` so the directionality rule (client→
  server MUST mask; server→client MUST NOT) is captured
  at the type level.
- `Opcode` — `Continuation` (0x0) / `Text` (0x1) /
  `Binary` (0x2) / `Close` (0x8) / `Ping` (0x9) /
  `Pong` (0xA). Reserved opcodes
  (0x3-0x7, 0xB-0xF) surface as `WsError::InvalidOpcode(u8)`
  at decode time.
- `CloseCode` — every RFC 6455 § 7.4.1 standard code
  named: `Normal` (1000) / `GoingAway` (1001) /
  `ProtocolError` (1002) / `UnsupportedData` (1003) /
  `InvalidPayload` (1007) / `PolicyViolation` (1008) /
  `MessageTooLarge` (1009) / `ExtensionRequired` (1010) /
  `InternalError` (1011). Plus `Custom(u16)` for IANA-
  registered and application-private ranges. Reserved
  codes (1004, 1005, 1006, 1015) rejected.
- `Message` + `MessageKind { Text, Binary }` — post-
  reassembly representation. v0.4 wires the reassembly
  logic.
- `Handshake` — `(request_key, accept_key, subprotocols,
  extensions)`. Carries both sides of the HTTP/1.1
  upgrade.
- `WsError` — failure modes: `Truncated` /
  `InvalidOpcode(u8)` / `ReservedBitSet` / `InvalidUtf8` /
  `InvalidCloseCode(u16)` / `PayloadTooLarge` /
  `MaskingMismatch` / `BadHandshake(&'static str)`.
- Entry-point stubs:
  - `encode_frame(&Frame, &mut Vec<u8>)`
  - `decode_frame(&[u8]) -> Result<(Frame, usize), WsError>`
  - `perform_client_handshake_request(host, path) -> String`
  - `parse_server_handshake_response(&[u8]) -> Result<Handshake, WsError>`
  - `parse_client_handshake_request(&[u8]) -> Result<Handshake, WsError>`
  - `build_server_handshake_response(&Handshake) -> String`
- `Cargo.toml` — full published metadata; `version = 0.1.0`
  (literal); no runtime deps (L9 v0.1 is API-only; v0.5
  picks up `grpcx-crypto` for SHA-1 + base64).
- `README.md`, `BUDGETS.md`, `WIRE_FORMAT.md`, this
  `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx-ws` as the
  tenth member.

### Gate
- `cargo test --workspace`: workspace builds clean. L9
  ships 2 smoke tests (`error_is_compact`,
  `frame_default_smoke`); workspace total **649 / 0**
  (was 647 at L8 v8.0; +2).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning across
  the `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check --all`: clean.
