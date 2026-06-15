# Changelog

All notable changes to `grpcx-crypto` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.3.1] below)

## [1.3.1] - 2026-06-12

Bugfix patch — **found by the new tonic-TLS interop gate**
(first time the byte-stream TLS face met a non-grpcx peer
with large payloads):

- `encrypt_app_data`: a single `write_all` of a >= 64 KiB
  chunk filled rustls's sendable-plaintext buffer
  (`set_buffer_limit` default 64 KiB) and failed; now
  interleaves writes with `write_tls` drains.
- `decrypt_app_data` / feed path: when the deframer
  refused input mid-burst, the unconsumed remainder was
  silently DROPPED; now feeds interleave with reader
  drains and only a genuine no-progress state errors.
- Regression KAT: 256 KiB app data both directions
  through a TlsConnection pair
  (`large_app_data_crosses_buffer_limit`).

## [1.3.0] - 2026-06-11

**Additive minor** — layer-up patch for L10's
`grpcx/h2`-over-TLS transport (T4): a network-driven
handshake loop needs to know when to promote without
consuming the handshake object (`into_connection` consumes
self even on failure).

### Added
- `ClientHandshake::is_complete` /
  `ServerHandshake::is_complete` — `true` once the
  handshake finished and `into_connection` will succeed.

## [1.2.0] - 2026-06-08

**Additive minor** — exposes `rustls::quic` per-encryption-level
keys for L4 `grpcx-quic` v0.6's Connection state machine.
Backwards compatible (nothing removed or changed); v1.0 / v1.1
callers unchanged. See
[../../REFACTOR-v4-v0.6-connection.md](../../REFACTOR-v4-v0.6-connection.md).

### Added
- `quic_handshake` module — `QuicEndpoint` wraps
  `rustls::quic::ClientConnection` / `ServerConnection`;
  `write_hs() -> (Vec<u8>, Option<QuicKeyChange>)` drains pending
  handshake bytes + reports key promotions; `read_hs(bytes)` feeds
  inbound. `QuicKeyChange::{Handshake, OneRtt}` deliver per-level
  `QuicKeys`.
- `QuicKeys` exposes `local_encrypt` / `remote_decrypt` (AEAD on
  the packet payload, in place) + `local_protect_header` /
  `remote_unprotect_header` (HP mask on first byte + PN bytes) +
  `sample_len()`.
- `QuicVersion` re-exported from `rustls::quic::Version`
  (`V1Draft` / `V1` / `V2`).
- One loopback handshake test
  (`tests/quic_handshake_loopback.rs`) — full client ↔ server
  drive, ALPN agreement, peer transport-params exchange, sees
  Handshake + 1-RTT key changes on each side.

### Note
- `Initial`-level keys are **not** delivered by this module —
  RFC 9001 §5.2 derives them from the client's destination
  Connection ID via [`quic::initial_secret`] + HKDF-Expand-Label
  (already shipped at v0.5 / v1.0). L4 keeps using that path for
  Initial packets; `quic_handshake` takes over from Handshake
  level onward.

## [1.1.0] - 2026-06-07

**Additive minor** — fills the one QUIC primitive the v1.0 surface
was missing: header-protection mask. Backwards compatible
(nothing removed or changed); driven by L4 `grpcx-quic` v0.3's
need for it on the packet protection / unprotection hot path. See
[../../REFACTOR-v4-v0.3-packet-protection.md](../../REFACTOR-v4-v0.3-packet-protection.md).

### Added
- `quic::HpSuite` enum — `Aes128` / `Aes256` / `ChaCha20`, the
  three QUIC v1 header-protection algorithms (RFC 9001
  §5.4.3-.5). 1:1 with the AEAD suites; the negotiated cipher
  suite picks both.
- `quic::HpError` enum — currently `BadKey` (HP key length or
  internal failure).
- `quic::header_protect_mask(suite, hp_key, sample) -> Result<[u8; 5], HpError>`
  — wraps `ring::aead::quic::HeaderProtectionKey::new_mask`.
  Returns the 5-byte mask (1 first-byte mask + 4 PN-mask bytes)
  per RFC 9001 §5.4.

## [1.0.0] - 2026-06-07

**L3 closed; semver-stable** (interim wrapper form). Project v3.0
release — `grpcx-crypto` joins `grpcx-io` and `grpcx-rt` on the
v1.x line. The v0.6 stabilisation + v0.7 close-out together meet
the lab-internal v3 trigger; this release is the SemVer anchor. See
[../../REFACTOR-v3-v0.7-close-out.md](../../REFACTOR-v3-v0.7-close-out.md)
and [../../REFACTOR-v3.0-release.md](../../REFACTOR-v3.0-release.md).

### Added
- `crates/grpcx-crypto/benches/compare_rustls.rs` — head-to-head
  bench harness against raw `rustls` + `ring` (4 benches in 3
  groups: `aead/`, `hkdf/`, `tls/`). Same dev-dep shape as L2's
  `compare_tokio`. The "≤ 5 % wrapper overhead" gate is the
  publication target (v1.x perf pass).

### Stability promise
- AEAD: `Aead` trait, `AeadError`, `Aes128Gcm`, `Aes256Gcm`,
  `ChaCha20Poly1305`.
- Kx: `Kx` trait, `KxError`, `X25519`, `P256`, `P384`.
- Sig: `Sig` trait, `SigError`, `Ed25519`, `EcdsaP256`,
  `RsaPkcs1` / `RsaPss` (still `unimplemented!()`; bodies land in
  a v1.x minor if needed).
- Hash family: `Hash` trait, `Sha256`, `Sha384`, `Sha512`.
- HMAC + HKDF: `Hmac<H>` (`HmacError`), `Hkdf<H>` (`HkdfError`;
  `extract` / `from_prk` / `expand` / `expand_label`).
- TLS 1.3: `TlsConfig` (`client` / `server` returning Result /
  `set_alpn` / `add_root_cert`), `ClientHandshake`,
  `ServerHandshake`, `TlsConnection`, `TlsError`.
- QUIC: `quic::INITIAL_SALT_V1`, `quic::PacketKeys`,
  `quic::initial_secret`, `quic::hp_secret`, `quic::key_iv_hp`.
- The `#[doc(hidden)]` `__fuzz_helpers` module is **not** part of
  the stable surface — may change or vanish in v1.x point releases.
  Same caveat as `grpcx-io` / `grpcx-rt`.

### Deferred (post-v1.0)
- Cross-OS `compare_rustls` publish + `PERFORMANCE.md` consolidation
  → v1.x perf pass (bench harness in place; only the measurement
  publication is deferred).
- `tests/perf_gate.rs` regression gate → v1.x perf pass.
- 24 h cumulative fuzz long-run → v1.x stabilisation cycle (on lx64,
  same host that closed L1 + L2 fuzz long-runs).
- **External cryptographer review pass** → post-external-window
  (gates the self-impl swap, not the wrapper).
- **Final self-implementation** (constant-time, hand-rolled) →
  post-external-review; the L3 charter trigger to retire `rustls`
  + `ring`. The API shape above is what the self-impl will keep —
  swap is drop-in.
- RSA suites (`RsaPkcs1` / `RsaPss`) bodies → v1.x minor if
  cert-chain verification through the wrapper surfaces a need.
- ≥ 1 internal lab project running on `grpcx-crypto` for 1 month
  → post-host-project.

## [0.6.0] - 2026-06-07

Stabilisation pass — coverage ≥ 95 %, fuzz harness in place, MEM/BINSIZE
numbers recorded, edge-case test coverage filled. Mirrors L2 v0.6's
shape. See
[../../REFACTOR-v3-v0.6-stabilisation.md](../../REFACTOR-v3-v0.6-stabilisation.md).

### Breaking
- `TlsConfig::server(cert_chain_der, private_key_der)` returns
  `Result<Self, TlsError>` (was `Self`, panicked on bad
  cert/key). Required so fuzz drivers can feed arbitrary bytes
  without crashing the process.

### Added
- `__fuzz_helpers` (`#[doc(hidden)] pub mod`) with byte-driven
  drivers: `drive_aead_open`, `drive_hkdf_expand_label`,
  `drive_tls_handshake_feed`, `drive_tls_server_feed`. Mirrors L1
  / L2's pattern.
- `crates/grpcx-crypto/fuzz/` subcrate with three fuzz targets:
  `aead_open`, `hkdf_expand_label`, `tls_handshake_feed` (the last
  alternates client + server feeds on the high bit of byte 0).
  Excluded from the main workspace (cargo-fuzz convention).
- `examples/min_tls.rs` — the canonical `cargo bloat --example
  min_tls` target for BINSIZE measurement. Mints a self-signed
  cert, runs an in-memory loopback handshake, exchanges one
  app-data record.
- `sizes` module with `size_of` const-asserts on the hash + sig
  shim types + a `report_internal_sizes` test that prints the full
  per-type table under `--nocapture`.

### Tests
- `tests/edge_cases.rs` (16 cases) — pushes coverage from 92.1 %
  → **95.9 % lines** by exercising public-surface arms missed by
  the v0.2-v0.5 KAT tests: bad-length AEAD seal/open, bad-nonce-len,
  premature `into_connection`, client/server config mismatches,
  `add_root_cert` on wrong side, ALPN setter on both sides, P-256 /
  P-384 `public_key` idempotency, ECDSA bad-public-len, and the
  RSA `unimplemented!()` panics (4 should_panic cases).
- `tests/kat_hmac.rs` extended with `Hmac<Sha384>::verify` and
  `Hmac<Sha512>::verify` positive cases.
- `tests/kat_hkdf.rs` extended with `Hkdf<Sha384>` / `Hkdf<Sha512>`
  round-trips and an `expand_label` overlong-output check.
- 4 `__fuzz_helpers` smoke tests.
- Total `grpcx-crypto` test count: **78 across 9 KAT files** (v0.2
  22 + v0.3 14 + v0.4 4 + v0.5 6 + v0.6 32).

### Coverage
- Workspace all-in: **95.90 % lines / 100 % functions** on the dev
  host (macOS aarch64), measured with `cargo-llvm-cov`. Trigger met.
- Uncovered arms: `unreachable!()` in the sealed-trait dispatches
  (3 lines, by design), a handful of TLS internal error-map arms
  (4 lines, exercised only by specific rustls error types), and
  the `decrypt_app_data` reader-EOF branch (2 lines).

### MEM / BINSIZE
- MEM: per-public-type `size_of` table recorded in
  `BUDGETS.md`. Highlights — `TlsConfig` 48 B, the three
  `*Handshake` / `TlsConnection` wrappers 16 B each (one boxed
  inner per side), Sig unit types 0 B (const-asserted), hash inner
  state 224 B (= ring's digest context, const-asserted).
- BINSIZE: `min_tls` example, `cargo bloat --release --crates`:
  total binary 1.4 MiB / .text 840.7 KiB; grpcx-crypto's own .text
  contribution is **11.9 KiB (1.4 %)**. rustls + ring + std
  dominate (82.0 %) — by design.

### Deferred
- 24h cumulative fuzz long-run — fuzz harness compiles + smokes
  clean; the long-run plan targets the same lx64 host that closed
  L2 v0.6's soak + fuzz, scheduled into v0.7 close-out alongside
  the `compare_rustls` runs.

## [0.5.0] - 2026-06-07

QUIC v1 key-derivation hooks land per RFC 9001 §5. L4
(`grpcx-quic`) can now derive its packet-protection keys
end-to-end through the L3 wrapper. See
[../../REFACTOR-v3-v0.5-quic-kdf.md](../../REFACTOR-v3-v0.5-quic-kdf.md).

### Added
- `quic::INITIAL_SALT_V1` — RFC 9001 §5.2 v1 initial salt as a
  20-byte `pub const`.
- `quic::initial_secret(connection_id)` — `HKDF-Extract(salt =
  INITIAL_SALT_V1, IKM = connection_id)`. Returns the 32-byte
  SHA-256 PRK bytes (the value `RFC 9001 § Appendix A.1` quotes).
- `quic::hp_secret(traffic_secret, key_len)` —
  `HKDF-Expand-Label(traffic_secret, "quic hp", "", key_len)`.
  Traffic secret used directly as PRK (already an HKDF output;
  not re-extracted).
- `quic::key_iv_hp(traffic_secret, key_len, iv_len, hp_len)` —
  three `HKDF-Expand-Label` calls with labels `"quic key"` /
  `"quic iv"` / `"quic hp"` and empty contexts. Returns a
  `PacketKeys { key, iv, hp }` triple.
- `Hkdf::<H>::from_prk(secret)` — new constructor that wraps raw
  PRK bytes (equivalent to `ring::hkdf::Prk::new_less_safe`). The
  TLS 1.3 / QUIC pattern needs this because the traffic secret is
  itself an HKDF output and must serve as the PRK input to a
  further `expand_label` without being re-extracted.

### Tests
- `tests/kat_quic.rs` — RFC 9001 Appendix A.1 vectors against the
  reference connection ID `0x8394c8f03e515708`: initial secret +
  client/server initial secrets + (key, IV, HP) per direction +
  `hp_secret` ↔ `key_iv_hp.hp` cross-check. 6 cases, all green.
- Total `grpcx-crypto` test count: **46 across 8 KAT files**
  (v0.2's 22 + v0.3's 14 + v0.4's 4 + v0.5's 6).

### Deferred (v3.x interim posture)
- QUIC v2 (RFC 9369) — different initial salt; surfaces as
  `quic::v2` later if L4 picks it up.
- SHA-384 cipher suites — hard-coded SHA-256 for now; generic-on-hash
  variants land if L4 needs them.

## [0.4.0] - 2026-06-07

TLS 1.3 connection wrapper lands. `TlsConfig` / `ClientHandshake` /
`ServerHandshake` / `TlsConnection` are now backed by `rustls 0.23`
(`ClientConfig` / `ServerConfig` / `ClientConnection` / `ServerConnection`).
The first checkpoint where the L3 wrapper talks TLS, not just
primitives. See
[../../REFACTOR-v3-v0.4-tls13.md](../../REFACTOR-v3-v0.4-tls13.md).

### Added
- `TlsConfig::client()` — empty trust store + empty ALPN.
- `TlsConfig::server(cert_chain_der, private_key_der)` — leaf-to-root
  DER chain + PKCS#8 v1 DER private key.
- `TlsConfig::set_alpn(&[&[u8]])` — preference-ordered ALPN list.
  Client side mutates the stored list; server side mutates the
  underlying `Arc<ServerConfig>::alpn_protocols` via `Arc::make_mut`.
- `TlsConfig::add_root_cert(&[u8])` — add a DER cert to the client
  trust store (**v3.x-only seam** — the final self-impl will have a
  different root-management API; this seam exists for testability
  and for self-signed loopback in v0.4-v0.6).
- `ClientHandshake::new(&TlsConfig, server_name)` —
  `take_handshake_bytes` / `process_handshake_bytes` /
  `into_connection`. Mirrors `ServerHandshake`.
- `TlsConnection::encrypt_app_data` / `decrypt_app_data` /
  `key_update` / `alpn_protocol`. Wraps `rustls::ConnectionCommon`
  via a small `ConnAccess` trait that erases Client vs Server.
- Sealed-trait infrastructure (`mod tls_impl`) now hosts an inner
  enum (`ConnInner::{Client(Box<...>), Server(Box<...>)}`) shared by
  the handshake + connection wrappers.

### Tests
- `tests/loopback_tls13.rs` — 4 cases against an in-process
  self-signed Ed25519 cert minted with `rcgen`:
  1. ALPN-negotiated `h3` handshake + app-data round-trip both
     directions.
  2. No-ALPN handshake.
  3. Untrusted-cert handshake fails with `TlsError::BadCert`.
  4. `key_update` smoke — app-data round-trips before and after.
- Total `grpcx-crypto` test count: **40 across 7 KAT files** (v0.2's
  22 + v0.3's 14 + v0.4's 4).

### Dependencies
- `rustls` — was already in deps from v0.1; v0.4 first uses it
  beyond the type-import sanity check. No version change.
- `rcgen = { workspace = true }` added as **dev-dependency only**
  (test cert minting). Same exception class as `criterion` /
  `mio` / `tokio` per the workspace `[workspace.dependencies]`
  rule. Forbidden in `src/`.

## [0.3.0] - 2026-06-07

`Kx` (X25519 / P-256 / P-384) + `Sig` (Ed25519 / ECDSA-P-256)
wrapper bodies land over `*ring*`. RSA suites stay
`unimplemented!()` for now (v0.4 if cert-chain verification needs
them). See
[../../REFACTOR-v3-v0.3-kx-sig.md](../../REFACTOR-v3-v0.3-kx-sig.md).

### Breaking
- `Kx::agree(&self, ...)` → `Kx::agree(self, ...)` — consumes the
  ephemeral private key on agreement. Ring's `agree_ephemeral` takes
  the private by value (no `Clone`), and consuming `self` also makes
  the "one DH per ephemeral key" forward-secrecy contract a compile-
  time guarantee. Charter-aligned refinement of the v0.1 frozen
  surface; no callers exist outside the crate so the change is safe.

### Added
- `Kx` impls — `X25519` / `P256` / `P384` over
  `ring::agreement::EphemeralPrivateKey`. Each per-curve type
  stores the private key plus a cached encoded public so
  `public_key(&self)` stays cheap and `agree(self)` does not need
  the public side. `kx_impl::alg<K>()` TypeId dispatch maps to ring's
  `X25519` / `ECDH_P256` / `ECDH_P384`.
- `Sig` impls — `Ed25519` + `EcdsaP256` over `ring::signature`:
  - `Ed25519`: `from_seed_unchecked` for sign (private = 32-byte
    raw seed; public = 32-byte raw; sig = 64-byte raw).
  - `EcdsaP256`: PKCS#8 v1 DER private + uncompressed SEC1 public +
    64-byte fixed-length `r || s` signature
    (`ECDSA_P256_SHA256_FIXED_SIGNING` / `ECDSA_P256_SHA256_FIXED`).
- `Sig` trait sealed (`: sealed::Sealed`) per the same private
  super-trait pattern used by `Hash` / `Aead` since v0.2.

### Tests
- `tests/kat_kx.rs` — round-trip + bad-public + `public_key`
  idempotency per curve (7 cases).
- `tests/kat_sig.rs` — Ed25519 RFC 8032 §7.1 Test 1 vector +
  tamper / bad-seed / bad-public, ECDSA-P-256 round-trip + tamper +
  bad-key (7 cases).
- Total: **36 tests across 6 KAT files** all pass (v0.2's 22 +
  v0.3's 14).

### Deferred (post-v0.3)
- `RsaPkcs1` / `RsaPss` bodies — v0.4 if rustls cert-chain
  verification through the L3 wrapper needs them.

## [0.2.0] - 2026-06-07

First implementation drop — wires the bottom half of the trait
surface (three AEAD suites, three SHA-2 hashes, HMAC, HKDF) over
`*ring*`. The trait API shape itself is **unchanged** from v0.1; only
the bodies land. See
[../../REFACTOR-v3-v0.2-aead-hash-hmac-hkdf.md](../../REFACTOR-v3-v0.2-aead-hash-hmac-hkdf.md).

### Added
- `Hash` impls — `Sha256` / `Sha384` / `Sha512` over
  `ring::digest::Context`. Stack-resident output (caller-provided
  `&mut [u8]` of length `H::OUTPUT_LEN`).
- `Hmac<H: Hash>` — generic over the hash, backed by
  `ring::hmac::Context`; `sign` emits the tag, `verify` is constant-
  time (small `ct_eq` helper since ring's
  `constant_time::verify_slices_are_equal` is now deprecated for
  external use).
- `Hkdf<H: Hash>` — `extract` / `expand` / `expand_label`. The
  `expand_label` op assembles the RFC 8446 §7.1 wire-format info
  string (`"tls13 "` prefix + length-prefixed label + length-
  prefixed context) and delegates to `expand`.
- `Aead` impls — `Aes128Gcm` / `Aes256Gcm` / `ChaCha20Poly1305` over
  `ring::aead::LessSafeKey`. Per-suite types thinly wrap a shared
  `KeyedAead` helper that does the length checks +
  `seal_in_place_separate_tag` / `open_in_place` plumbing. Zero
  alloc on the hot path (caller's `in_out` buffer carries
  ciphertext + tag scratch).
- Sealed `Aead` / `Hash` traits via private super-trait
  `sealed::Sealed`. External `impl` of these traits is not part of
  the stable API; pick one of the shipped suites/hashes.

### Tests
- `tests/kat_hash.rs` — SHA-256 / 384 / 512 `abc` + empty + a
  sliced-vs-oneshot equivalence sanity check (5 cases).
- `tests/kat_hmac.rs` — RFC 4231 case 1 over SHA-256 / 384 / 512 +
  verify acceptance / bad-tag / bad-length checks (6 cases).
- `tests/kat_hkdf.rs` — RFC 5869 Appendix A.1 / A.2 / A.3 over
  SHA-256 + a cross-check that `expand_label` produces the same OKM
  as `expand` over a hand-assembled `tls13`-prefixed info string
  (4 cases).
- `tests/kat_aead.rs` — round-trip + tamper + bad-key-len for each
  suite; NIST CAVP empty-PT vectors for AES-128-GCM /
  AES-256-GCM; RFC 8439 §2.8.2 "Ladies and Gentlemen..." canonical
  vector for ChaCha20-Poly1305 (7 cases).

### Dependencies
- `ring = { workspace = true }` added as a direct dep. Same
  time-bounded 0-dep exception as `rustls` (see
  [`../../DEPS_AUDIT.md`](../../DEPS_AUDIT.md) § L3 crypto interim
  wrapper). Already transit-pulled via `rustls`'s `["ring"]`
  feature; the direct dep is so `grpcx-crypto` can name ring's
  AEAD / digest / HMAC / HKDF primitives at the wrapper trait impl
  level.

## [0.1.0] - 2026-06-07

L3 bootstrap — `grpcx-crypto` crate skeleton, API trait design,
design docs synced. The four-doc bootstrap pattern that L1 + L2
followed (`ROADMAP § Lx` + `DEPS_AUDIT § Lx` + `docs/vN-scope.md` +
`REFACTOR-vN-v0.1-bootstrap.md`). See
[../../REFACTOR-v3-v0.1-bootstrap.md](../../REFACTOR-v3-v0.1-bootstrap.md).

### Added
- `src/lib.rs` API skeleton — trait surface (`Aead`, `Kx`, `Sig`,
  `Hash`, `Hmac`, `Hkdf`) + concrete suite/curve/sig/hash types
  (`Aes128Gcm`, `Aes256Gcm`, `ChaCha20Poly1305`; `X25519`, `P256`,
  `P384`; `Ed25519`, `EcdsaP256`, `RsaPkcs1`, `RsaPss`; `Sha256`,
  `Sha384`, `Sha512`) + TLS 1.3 connection wrapper (`TlsConfig`,
  `ClientHandshake`, `ServerHandshake`, `TlsConnection`, `TlsError`)
  + QUIC key derivation module (`quic`). Every item carries a doc
  comment; bodies are `unimplemented!()` — the v0.2-v0.5 checkpoints
  drop in the rustls-backed bodies one trait family at a time.
- `BUDGETS.md` — performance budget taxonomy (HOT / WARM / COLD)
  for the hot paths that will land in v0.2-v0.5. Numbers are TBD
  until the corresponding backend exists.

### Dependencies
- `grpcx-io` (path = "../grpcx-io"), `grpcx-rt` (path = "../grpcx-rt")
  — lower-layer `grpcx-*` only.
- `rustls` (workspace dep, `features = ["std", "ring"]`) — documented
  time-bounded exception per [`../../DEPS_AUDIT.md`](../../DEPS_AUDIT.md)
  § L3 crypto interim wrapper. Retire-when: external cryptographer
  review window opens AND self-impl passes BoGo + NIST CAVP.
