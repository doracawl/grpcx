# `grpcx-crypto` Performance Budgets

Hot / warm / cold path budgets, enforced by `tests/perf_gate.rs` once
backends land (perf_gate wiring planned for v0.6+ once a baseline
exists). CI skips these with `--skip _under_budget`; run locally on
a quiet machine.

Project priority order is **perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `rustls` (`compare_rustls.rs`, lands v0.7).

## v0.1 status

API surface only — no implementation, so **no observed numbers yet**.
This file fixes the path taxonomy so each v0.x checkpoint fills its
rows as it lands. All `Observed` / `Headroom` cells are TBD until the
corresponding backend exists.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | `Aead::seal_in_place` | v0.2 | One AEAD encrypt over a small record (≤ 16 KiB); zero alloc |
| HOT | `Aead::open_in_place` | v0.2 | One AEAD decrypt + auth check; zero alloc |
| HOT | `Hash::update` + `finalize` | v0.2 | One hash over a small record; output is `[u8; OUTPUT_LEN]` (stack) |
| HOT | `Hmac::sign` / `Hmac::verify` | v0.2 | One MAC over a small record |
| HOT | `Hkdf::expand_label` (TLS / QUIC) | v0.2 / v0.5 | One label expansion (RFC 9001 §5) |
| HOT | TLS 1.3 record encrypt / decrypt (`TlsConnection::encrypt_app_data` / `decrypt_app_data`) | v0.4 | One application-data record on the steady-state path |
| WARM | `Kx` keypair generation | v0.3 | One ephemeral keypair per handshake |
| WARM | `Kx::agree` | v0.3 | One ECDH agreement per handshake |
| WARM | `Sig::sign` / `Sig::verify` | v0.3 | One signature per handshake (server) / per cert (client) |
| WARM | TLS 1.3 handshake (`ClientHandshake::progress` / `ServerHandshake::progress`) | v0.4 | One full handshake round-trip |
| WARM | `TlsConnection::key_update` | v0.4 | One key-update rotation |
| COLD | `TlsConfig` construction | v0.4 | Cert chain parsing + ALPN setup |
| COLD | Error paths | v0.2+ | Bad MAC / bad cert / bad handshake → typed `TlsError` |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_rustls`) |

## Memory budgets (MEM tier)

Per-instance footprint of the public types, observed via
`sizes::report_internal_sizes` (mirrors L1 / L2 pattern). Hash /
HMAC / HKDF / AEAD shim types pass straight through to ring's
inner state; their sizes are platform-dependent and recorded, not
const-asserted. Wrapper types (`TlsConfig`, `*Handshake`,
`TlsConnection`) carry at most one `Box`/`Arc` pointer plus a small
enum discriminant — the wrapper's own overhead over rustls is
sub-pointer.

Observed on the dev host (macOS aarch64) at v0.6:

| Type | Size | Notes |
|---|---:|---|
| `Sha256` / `Sha384` / `Sha512` | 224 B | `ring::digest::Context` |
| `Hmac<Sha256>` | 312 B | `ring::hmac::Context` (carries the HMAC inner key state) |
| `Hkdf<Sha256>` | 176 B | `ring::hkdf::Prk` + zero-sized hash marker |
| `Aes128Gcm` / `Aes256Gcm` / `ChaCha20Poly1305` | 560 B | `ring::aead::LessSafeKey` (per-suite expanded key) |
| `X25519` / `P256` / `P384` | 88 B | `ring::agreement::EphemeralPrivateKey` + cached encoded public `Vec<u8>` |
| `Ed25519` / `EcdsaP256` / `RsaPkcs1` / `RsaPss` | 0 B | unit type — sign/verify are static methods, no state |
| `TlsConfig` | 48 B | `enum { Client { RootCertStore, Vec<Vec<u8>> }, Server(Arc<ServerConfig>) }` discriminant + larger variant |
| `ClientHandshake` / `ServerHandshake` / `TlsConnection` | 16 B | `enum { Client(Box<...>), Server(Box<...>) }` discriminant + boxed inner |

Const-asserts pin the Hash inner-state width to `ring::digest::Context`
and the Sig unit types to 0 B so a regression on those rows is a
compile error.

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via `cargo bloat
--release --example min_tls --crates` against a canonical
`examples/min_tls.rs` (construct a `TlsConfig`, run a loopback
handshake, encrypt + decrypt one application record). Regression
budget: ≤ 5 % growth per minor is noise; ≥ 10 % stops the release.

| Version | Total binary | `grpcx-crypto` contribution | Notes |
|---|---:|---:|---|
| v0.1 | — | — | API-only; no example binary yet |
| v0.6 | 1.4 MiB | **11.9 KiB `.text` (1.4 % of binary)** | macOS aarch64 release (fat LTO, stripped). Of the .text 840.7 KiB total: std 293.2 KiB (34.9 %), rustls 256.8 KiB (30.5 %), ring 138.4 KiB (16.5 %), rcgen 40.4 KiB (4.8 % — dev-only, but the `min_tls` example mints a cert), webpki 35.3 KiB (4.2 %), grpcx-crypto **11.9 KiB (1.4 %)**. The wrapper's own contribution is small; rustls + ring dominate — by design. |
