# Changelog

All notable changes to the `grpcx` project are documented in this file.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`grpcx` is a from-scratch RPC stack built as ten separately versioned
crates, layered from the OS reactor up to the gRPC façade. Each crate
below has its own H2 release section; entries under one crate are
ordered newest-first.

---

## grpcx 0.25.0 — 2026-06-12

The top-level RPC façade: unary / server-streaming / client-streaming /
bidi over `grpcx/h3`, h2c, h2 + TLS, `grpcx/tcp-mux`, `grpcx/ws-mux`,
and `grpcx/ws-mux` + TLS. Project v10.0 was released 2026-06-10.

### Added
- `Server::set_max_connection_idle(Option<Duration>)` — passive idle-
  connection GC; a serve-loop connection with no inbound traffic for
  the window is reaped by its own drive task (default `None`).
- `ConnectedChannel::connect_ws_mux_tls(addr, tls, server_name,
  authority)` and `Server::listen_ws_mux_tls[_until]` — the
  `grpcx/ws-mux` grammar over TLS 1.3 (`transport-ws-mux` +
  `transport-h2-tls`).
- `ConnectedChannel::connect_ws_mux(addr, authority)` /
  `Server::listen_ws_mux[_until]` — `grpcx/ws-mux` over real TCP
  sockets; full four-shape RPC parity with the other transports.
- `ConnectedChannel::connect_h2(addr, tls, server_name, authority)` /
  `Server::listen_h2[_until]` — h2 over TLS 1.3 (`transport-h2-tls`),
  built on `TlsTransport` middleware.
- `ConnectedChannel::connect_h2c(addr, authority)` /
  `Server::listen_h2c[_until]` — cleartext prior-knowledge HTTP/2
  bridge, wire-compatible with `tonic` `http://` endpoints
  (`transport-h2`).
- `ConnectedChannel::connect_tcp_mux(addr, authority)` /
  `Server::listen_tcp_mux[_until]` — `grpcx/tcp-mux`, the bare-metal
  transport (`transport-tcp-mux`); same mux frame grammar as ws-mux
  over raw TCP.
- `Channel::set_link_liveness(keepalive, idle_timeout)` and per-
  connection setters on every server-conn type; dispatches to the
  negotiated transport (defaults 2.5 s / 10 s).
- `CallContext::set_timeout(Duration)` and end-to-end enforcement of
  the standard `grpc-timeout` header on every blocking facade; expiry
  yields `Status(DeadlineExceeded)` as a per-call verdict (connection
  survives).
- Eager cancel on deadline expiry: every transport emits a wire-level
  cancel (QUIC RESET_STREAM + STOP_SENDING for h3; RST_STREAM(CANCEL)
  for h2/h2c; new `MUX_RST` frame for tcp-mux / ws-mux).
- Multi-core `Server::listen_h3[_until]` — UDP router thread plus
  task-per-connection on a `grpcx-rt` runtime; a slow handler no
  longer head-of-line blocks other connections.
- L2-runtime TCP serve loop (`listen_h2c` / `listen_tcp_mux` /
  `listen_h2`): event-driven, multi-core, task-per-connection;
  `Service` trait objects are now `+ Send + Sync` and `Server` is
  `Clone`.
- `DatagramTransport::send_batch` / `recv_batch` — UDP batch IO on
  Darwin + Linux (`sendmmsg_x` / `sendmmsg`), with per-platform
  overrides on `UdpTransport`.
- `Channel::set_datagram_budget` / `ServerConn::set_datagram_budget` —
  configurable QUIC 1-RTT plaintext budget (default raised from 1100
  to 1350; loopback can go to ~9000).
- ECONNREFUSED policy on connected UDP: fast-fail
  `"udp: connection refused"` during handshake; swallowed as transient
  on established connections (liveness owns the death verdict).
- Reactor-park drive loop: socket-backed loops now park in
  `grpcx-io`'s reactor when idle (kqueue / epoll), waking immediately
  on incoming datagrams.
- ALPN selection (`select_alpn_transport`, `grpcx/h3` preferred,
  `grpcx/ws-mux` fallback) and `Alt-Svc` codec.
- Typed service stubs from `.proto` via the `proto!` / `proto_inline!`
  macros — no `protoc`, no `build.rs`. Consumers depend on `grpcx` +
  `grpcx-pb` only.
- `CallContext` — symmetric per-call context (request metadata out,
  trailing metadata back) replacing the `Request<T>`/`Response<T>`
  wrapper noise.
- `ConnectedChannel` — channel + transport bundled, exposing the
  tonic-habit `call_*` blocking methods.
- Server-streaming, client-streaming, and bidirectional streaming on
  every wired transport; `GrpcTimeout` + `TimeoutUnit` wire codec.
- gRPC wire framing (`MessageFrame` / `encode_frame` / `decode_frame`),
  HTTP/3-shaped header and trailer codecs with `grpc-status` /
  `grpc-message` percent-coding, and the public ADTs (`Service`,
  `Method`, `RpcKind`, `Status`, `StatusCode`, `Metadata`, `RpcError`,
  `TransportKind`, `Channel`, `Server`, `call_unary`).

### Fixed
- Whole-stack O(calls²) growth: every RPC leaked one dead stream entry
  in QUIC + H/3; now closed via `close_request` / `close_stream` wired
  into all completion paths.
- `TcpTransport::send` could fail mid-write on `WouldBlock` when a
  chunk exceeded the kernel send buffer (tcp-mux tripped it); now a
  spin-yield write loop with stall cap.
- `grpcx-h2`: trailing HEADERS could be emitted ahead of window-
  buffered DATA, which strict peers (tonic) rejected; trailers now
  queue behind data and carry END_STREAM.
- `call_unary` drive loop: now batch-drains the receive queue and
  spins briefly before yielding.

### Performance
- Live bidirectional interop with `tonic` over h2c and h2 + TLS, plus
  the streaming RPC shapes verified in both directions.
- Tonic-TLS interop gate in place; cross-stack head-to-head benches
  consolidated under `comparators/tonic-grpcx`.

---

## grpcx-h2 0.3.0 — 2026-06-12

### Added
- `send_rst_stream(stream, code)` — eager cancel; writes to a reset
  stream become no-ops via a bounded recently-reset ring.
- `pump_at(incoming, now_us)` — caller-injected monotonic clock for
  the sans-IO time surface. `pump` is unchanged.
- Link liveness: a quiet link past the keepalive interval (default
  2.5 s, `set_keepalive_us`) emits an RFC 9113 PING; no inbound past
  the idle timeout (default 10 s, `set_idle_timeout_us`) yields
  `H2Error::LinkDead`.
- Initial release: RFC 9113 framing, full HPACK, connection state
  machine (preface / SETTINGS / streams / HEADERS + CONTINUATION,
  two-level flow control). This is the h2c interop-bridge core.

---

## grpcx-h3 1.8.0 — 2026-06-12

### Added
- `cancel_request(rs)` — emits QUIC RESET_STREAM + STOP_SENDING with
  `H3_REQUEST_CANCELLED` (0x10c) and drops local request state; the
  pump now drains L4 reset events.
- `set_link_liveness_us(keepalive_us, idle_timeout_us)` — passthrough
  to `grpcx-quic`'s link-liveness atom.
- `pump_at(incoming, now_us)` — threads the caller's monotonic clock
  through to `grpcx-quic`'s sans-IO time surface.
- `Connection::set_datagram_budget` — passthrough to the configurable
  QUIC 1-RTT plaintext budget.
- Trailing HEADERS support — the second HEADERS frame on a request
  stream is now stored as the trailing section (RFC 9114 §4.1
  `HEADERS DATA* HEADERS`); `Connection::read_trailers` /
  `received_trailers_ready` mirror `read_headers`. Sending trailers
  is a second `write_headers` before `finish`.
- `Connection::close_request` — releases the H/3 `RequestState` and
  the underlying QUIC stream entry when the caller is done.
- `Connection::send_h3_datagram` / `recv_h3_datagram` — RFC 9297 H/3
  datagram wrapper around `grpcx-quic`'s DATAGRAM frame API.
- Upper-layer raw-stream surface (`accept_unknown_uni_stream`,
  `accept_unknown_bi_stream`, `read_stream_raw`, `write_stream_raw`,
  `open_uni_raw`, `open_bi_raw`) — lets WebTransport claim peer-
  initiated reserved-typed uni streams and peer bidis out of the
  H/3 request path.
- Full QPACK dynamic-table support — encoder/decoder instruction
  codecs (RFC 9204 §4.3 + §4.4) plus dynamic-aware field-section
  decode (all five RFC 9204 §4.5 patterns including Post-Base);
  inbound HEADERS now resolve dynamic references, with section
  acknowledgements emitted automatically.
- H/3 Connection state machine — lazily opens control + QPACK uni-
  streams once 1-RTT is reached, parses SETTINGS / GOAWAY /
  MAX_PUSH_ID / CANCEL_PUSH / PRIORITY_UPDATE / ORIGIN, drives
  request streams, and exposes `send_request` / `accept_request` /
  `write_headers` / `write_data` / `finish` / `read_headers` /
  `read_data` / `goaway`.
- Stream-type codec (`StreamType` enum, `read_stream_type` /
  `write_stream_type`, `is_critical`, `is_unique_per_direction`) for
  RFC 9114 §6.2 unidirectional streams.
- QPACK static-table encode + decode (RFC 9204) — full 99-entry
  static table plus prefixed-integer codec.
- HTTP/3 frame codec — encode + decode for every RFC 9114 §7.2 wire
  frame (DATA / HEADERS / CANCEL_PUSH / SETTINGS / PUSH_PROMISE /
  GOAWAY / MAX_PUSH_ID / ORIGIN / PRIORITY_UPDATE / Reserved).

### Fixed
- `read_prefixed_int` 8-bit prefix shift-overflow on the Required
  Insert Count field, surfaced the first time QPACK was called with
  an 8-bit prefix.

### Performance
- `drain_one_request` now uses a deferred-drain cursor — one drain
  per call instead of one `Vec::drain(..)` per frame.
- Per-request buffer recycling: `close_request` harvests `in_buf` /
  `body` capacity into a bounded spare pool; new requests draw from
  it.
- `write_data` emits the DATA frame header directly and appends the
  payload as a second stream write (one copy instead of three);
  receive-side fast path streams DATA payloads straight into the
  body buffer.

---

## grpcx-quic 1.9.1 — 2026-06-12

### Added
- `cancel_stream(sid, code)` — emits RESET_STREAM for our send half
  (with pending data and retransmit spans purged) plus STOP_SENDING
  for the peer's; `take_reset_streams()` surfaces ended streams to
  the upper layer. Receive paths honour RESET_STREAM / STOP_SENDING
  per RFC 9000 §3.5, with late in-flight data dropped via a bounded
  recently-reset ring.
- Cancel-frame loss recovery: RESET_STREAM / STOP_SENDING now ride
  each packet's `SentRecord` and re-queue (front) on packet-threshold
  loss or PTO harvest per RFC 9000 §13.3.
- Link liveness: keepalive PINGs (default 2.5 s, `set_keepalive_us`)
  on a quiet link, idle-timeout death (default 10 s,
  `set_idle_timeout_us`) on no inbound, pump-gap forgiveness for
  blocking callers. All anchored to the sans-IO clock.
- CUBIC congestion control (RFC 9438) as the new default, with
  NewReno (RFC 9002 §7) preserved as `CongestionAlgorithm::NewReno`;
  `set_congestion_algorithm()` selects. Persistent-congestion
  collapse (RFC 9002 §7.6) included.
- `pump_at(incoming, allow_pure_ack, now_us)` — sans-IO time surface
  with caller-injected monotonic µs clock. Drives RTT estimation
  (RFC 9002 §5), PTO / tail-loss probes (RFC 9002 §6.2), handshake-
  plane CRYPTO retransmit, and CRYPTO receive reassembly.
- `next_wakeup_us()` — earliest armed deadline for drivers that want
  to bound a park.
- ACK generation / processing, retransmit buffering, packet-threshold
  loss recovery (RFC 9002 §6.1.1, `kPacketThreshold = 3`); ACKs
  piggyback on outbound packets, PURE-ACK packets are emitted only on
  truly idle pumps. `pump_with(incoming, allow_pure_ack)` exposes the
  control.
- `Connection::set_datagram_budget(plaintext_budget)` — configurable
  1-RTT plaintext budget (default raised from 1100 to 1350).
- `Connection::close_stream` — soft-close; the entry is reaped once
  its send queue fully drains onto the wire.
- `QuicConnection::send_datagram` / `recv_datagram` — RFC 9221
  DATAGRAM frame plumbing wired into the connection state machine.
- `QuicConnection::open_uni` / `accept_uni` / `accept_bi` — RFC 9000
  §2.1 unidirectional stream open and peer-initiated stream
  enumeration.
- `QuicConnection` — full connection state machine with handshake to
  1-RTT and bidirectional STREAM round-trip; `new_client` /
  `new_server` / `pump` / `open_bi` / `write_stream` / `read_stream`.
- `initial` module — `build_initial_packet` / `parse_initial_packet`
  with RFC 9000 §14.1 first-flight padding (`padded_to`); composes
  varint, header codec, frame codec, and packet protection.
- Frame codec — `Frame` enum covering every RFC 9000 §19 frame plus
  RFC 9221 DATAGRAM; `encode_frame` / `decode_frame` /
  `frame_varint_len`.
- Packet protection — `AeadSuite` (`Aes128Gcm` / `Aes256Gcm` /
  `ChaCha20Poly1305`), in-place `protect_payload` /
  `unprotect_payload`, `apply_header_protection` /
  `remove_header_protection`.
- Packet headers and varint — RFC 9000 §16 varint codec and
  `LongHeader` / `ShortHeader` / `PacketHeader` /
  `LongHeaderType` + `encode_header` / `decode_header`.

### Fixed
- `decode_ack` unbounded `range_count` (OOM on adversarial input) —
  now capped by remaining buffer length before `Vec::with_capacity`.
- `build_one_rtt_datagram` packed all pending stream data into a
  single datagram; now chunked to the per-datagram plaintext budget
  with the pump looping until queues drain. Indivisible DATAGRAM
  frames are packed while they fit, oversized ones go out alone.

### Performance
- 1-RTT frames encode directly into the packet buffer
  (`begin_short_packet` / `seal_short_packet`); one allocation and
  one full-payload memmove per packet eliminated.
- `StreamState::pending_send` is read by a cursor instead of front-
  drained; `build_short_packet`'s output buffer is sized exactly up
  front.
- Send-side queue slices encode straight into the packet plaintext
  via a borrowed-payload STREAM encoder; receive-side short-packet
  STREAM frames parse in place and extend `recv_buf` directly.

---

## grpcx-crypto 1.3.1 — 2026-06-12

### Added
- `ClientHandshake::is_complete` / `ServerHandshake::is_complete` —
  the network-driven handshake loop can now query whether
  `into_connection` will succeed without consuming the handshake.
- `quic_handshake` module — `QuicEndpoint` wraps `rustls::quic::
  ClientConnection` / `ServerConnection`; `write_hs() -> (Vec<u8>,
  Option<QuicKeyChange>)` drains handshake bytes and reports key
  promotions, `read_hs(bytes)` feeds inbound. `QuicKeys` exposes
  `local_encrypt` / `remote_decrypt` plus `local_protect_header` /
  `remote_unprotect_header`; `QuicKeyChange::{Handshake, OneRtt}`
  delivers per-level keys.
- `quic::HpSuite` (`Aes128` / `Aes256` / `ChaCha20`) and
  `quic::header_protect_mask(suite, hp_key, sample) -> Result<[u8; 5],
  HpError>` — the QUIC v1 header-protection mask (RFC 9001 §5.4).
- `quic::INITIAL_SALT_V1`, `quic::initial_secret`, `quic::hp_secret`,
  `quic::key_iv_hp` — RFC 9001 §5 QUIC v1 key derivation hooks plus
  `Hkdf::<H>::from_prk` for TLS 1.3 / QUIC traffic-secret expansion.
- TLS 1.3 connection wrapper — `TlsConfig::{client, server, set_alpn,
  add_root_cert}`, `ClientHandshake` / `ServerHandshake` /
  `TlsConnection` with `encrypt_app_data` / `decrypt_app_data` /
  `key_update` / `alpn_protocol`. Backed by `rustls 0.23` (interim
  wrapper form).
- `Kx` trait + impls (`X25519` / `P256` / `P384`) over
  `ring::agreement::EphemeralPrivateKey`; `Sig` trait + impls
  (`Ed25519` / `EcdsaP256`).
- AEAD (`Aes128Gcm` / `Aes256Gcm` / `ChaCha20Poly1305`), hashes
  (`Sha256` / `Sha384` / `Sha512`), `Hmac<H>`, `Hkdf<H>` with
  `extract` / `from_prk` / `expand` / `expand_label` — full bottom
  half of the trait surface.

### Changed
- `TlsConfig::server(cert_chain_der, private_key_der)` returns
  `Result<Self, TlsError>` (was infallible and panicked on bad
  cert/key) — required for fuzz drivers and for honest reporting of
  malformed input.
- `Kx::agree(self, ...)` (was `&self`) — agreement now consumes the
  ephemeral private key, making the "one DH per ephemeral" forward-
  secrecy contract a compile-time guarantee.

### Fixed
- `encrypt_app_data`: a single `write_all` of a ≥ 64 KiB chunk filled
  rustls's sendable-plaintext buffer and failed; writes now interleave
  with `write_tls` drains.
- `decrypt_app_data` feed path: when the deframer refused input mid-
  burst the unconsumed remainder was silently dropped; feeds now
  interleave with reader drains and a real no-progress state errors
  explicitly.

---

## grpcx-pb 1.5.0 — 2026-06-13

`.proto` editions-2023 importer with proto2 and proto3 front-ends.

### Added
- proto2 dialect (`Dialect::Proto2`): `syntax = "proto2";` accepted;
  `optional` maps to the explicit-presence kernel (`Option<T>`,
  `Some(default)` on the wire); `required` maps to `Label::Singular`;
  `repeated` maps to `Label::Repeated`. Label-less regular message
  fields are rejected (protoc parity); `extensions` and labeled
  `group` surface as `UnsupportedFeature`. `[default = …]` is parsed
  and discarded.
- proto3 explicit-presence kernel: `Label::Optional`. The `optional`
  field prefix generates `Option<T>` codegen; `Some(default)` goes on
  the wire to distinguish from `None`; `optional repeated` and
  `optional map<…>` are rejected.
- proto3 front-end (`Dialect::Proto3`): `syntax = "proto3";` accepted;
  enum first-value-zero enforced; cross-checked against `prost` for
  wire compatibility.
- `Schema.services: Vec<ServiceDef>` plus `ServiceDef` / `MethodDef`;
  full `service` / `rpc` body grammar including dotted type names and
  service-level options. Typed service-stub codegen emits
  `XxxClient`, `XxxHandler` trait, and `XxxServer<T>` implementing
  `::grpcx::Service`.
- `ProtoCodec` trait and codegen — generated structs gain `impl
  ::grpcx_pb::ProtoCodec`; generated enums gain `proto_value` /
  `from_proto_value` mapping helpers.
- Wire-format primitives (`src/wire.rs`): `WireType`, varint codec,
  zigzag (sint32 / sint64), fixed32 / fixed64, length-delimited,
  tag, `skip_field`, `to_proto_bytes` / `from_proto_bytes`.
- `codegen(&Schema) -> String` — emits Rust source: messages as
  derived structs, enums as derived enums, packages as nested `pub
  mod`s, nested message types in snake-cased modules. Includes Rust
  keyword escaping.
- Semantic analyser — symbol-table-driven reference resolution,
  `FieldType::Message` vs `Enum` reclassification, fully-qualified
  name rewriting, field-number range checks (`1..=536_870_911` minus
  `19000..=19999`), duplicate detection.
- Parser — recursive-descent over the token stream into the public
  `Schema` ADTs; full `message` / `enum` / `oneof` (collapsed) /
  `package` / `import` grammar.
- Lexer + syntax-line gate — full editions-2023 tokenizer; rejects
  `syntax = "proto2"` / `"proto3"` at the gate when no front-end is
  active (later replaced by the proto2/proto3 front-ends).
- `import(source: &str) -> Result<Schema, ImportError>` — single
  entry point; `Schema`, `Message`, `Field`, `Label`, `FieldType`
  (15 scalars + `Message` / `Enum` / `Map`), `Enum`, `EnumValue`,
  `ImportError`.

### Changed
- Generated code now derives `Default` and references
  `::grpcx_pb::alloc::…` instead of `::alloc::…` so it compiles in
  `std` consumer crates without `extern crate alloc`.
- Default derives switched to the prost-habit set (`Debug, Default,
  Clone, PartialEq`); the `GrpcxIdl` derive is opt-in. Consumers of
  generated code now depend on exactly two crates: `grpcx` +
  `grpcx-pb`.

---

## grpcx-pb-derive 0.1.0 — 2026-06-11

### Added
- `proto!` / `proto_inline!` proc-macros — `.proto` → typed Rust API
  with no `protoc`, no `build.rs`.

---

## grpcx-ws 1.1.0 — 2026-06-10

WebSocket framing and RFC 6455 § 1.3 handshake, plus extension
negotiation.

### Added
- `ExtensionOffer` + `parse_extension_header` /
  `parse_extension_offer` / `format_extension_header` /
  `find_offer_by_name` — quote-aware RFC 6455 §9.1
  `Sec-WebSocket-Extensions` codec.
- `PermessageDeflateOffer` — strongly-typed RFC 7692 offer with
  `parse(&ExtensionOffer)` / `to_offer(&self)`.
- HTTP/1.1 upgrade handshake builders + parsers:
  `perform_client_handshake_request`,
  `parse_server_handshake_response`,
  `parse_client_handshake_request`,
  `build_server_handshake_response`, plus `derive_accept_key` and
  `verify_accept_key` (RFC 6455 §4.2.2 step 5).
- `Reassembler` + `ReassemblyOutput` — RFC 6455 §5.4 message
  reassembly with control-frame pass-through and on-emit UTF-8
  validation (§5.6).
- `CloseCode::from_u16` / `to_u16` / `is_reserved`,
  `encode_close_payload` / `decode_close_payload` — RFC 6455 §7
  close-payload codec.
- Frame codec (`encode_frame` / `decode_frame`) — RFC 6455 §5.2
  with smallest-form enforcement on extended lengths and the §5.7
  masking reference vector locked.
- `Frame`, `Opcode`, `Message`, `MessageKind`, `Handshake`,
  `WsError` (`Truncated` / `InvalidOpcode` / `ReservedBitSet` /
  `InvalidUtf8` / `InvalidCloseCode` / `PayloadTooLarge` /
  `MaskingMismatch` / `BadHandshake` / `ProtocolError` /
  `FragmentedControlFrame`).

### Changed
- The internal `Cursor`, SHA-1, base64, and HTTP token helpers were
  lifted into the in-house 0-runtime-dep `stones` workspace crates
  (`byte-cursor`, `sha1-rfc3174`, `base64-rfc4648`, `http1-syntax`);
  the public surface is unchanged.

---

## grpcx-wt 1.0.0 — 2026-06-09

WebTransport (RFC 9220) session layer over HTTP/3.

### Added
- `Session` — WebTransport session over an H/3 connection: `open` /
  `accept` / `is_ready` / `open_bi` / `open_uni` / `accept_bi` /
  `accept_uni` / `send_datagram` / `recv_datagram` / `close` /
  `session_id`.
- `SessionMux` — multi-session dispatch over one H/3 connection
  (`new` / `insert` / `get` / `get_mut` / `remove` / `iter` /
  `poll`). Streams whose session id doesn't match are silently
  dropped.
- Capsule framer — `encode_close_capsule(code, reason, out)`,
  `decode_capsule`, `Capsule { Close, Unknown }`,
  `CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION = 0x2843`; rejects
  reasons > 1024 bytes (RFC 9220 §5.3).
- `Session::send_datagram` / `recv_datagram` — per-session unreliable
  datagrams; cross-session payloads surface as `Error::Internal` on
  single-session deployments.
- Stream dispatch — `Session::open_bi` / `open_uni` write the
  `[0x54][session_id]` prefix automatically; `Session::poll` claims
  peer-initiated WT streams out of the H/3 path and routes by
  session id.
- Extended CONNECT bootstrap (RFC 9220 §3 + RFC 8441) —
  `build_connect_request_headers`, `build_connect_response_headers`,
  `validate_connect_request`, `parse_response_status`,
  `Session::open` / `Session::poll_open` / `Session::accept`.
- WT framing — `WT_STREAM_TYPE: u64 = 0x54`,
  `write_wt_stream_prefix` / `read_wt_stream_prefix`,
  `write_wt_datagram` / `read_wt_datagram` (RFC 9297 H/3 datagram
  shape, `quarter_stream_id = session_id / 4`).
- `BidirectionalStream`, `UnidirectionalStream`, `Error` (`H3` /
  `ConnectProtocolNotAdvertised` / `ConnectRejected` /
  `BadStreamFrame` / `BadDatagram` / `SessionClosed` / `Internal`).

---

## grpcx-idl 1.0.0 — 2026-06-09

Zero-copy binary IDL — own wire format, no runtime dependencies.

### Added
- `Archive` / `Serialize<W>` / `Deserialize<D>` trait surface plus
  `Writer`, `to_bytes`, `from_bytes`, `Error` (`Truncated` /
  `BadLayout` / `BadAlignment` / `Utf8` / `Custom`).
- Primitive codec for `u8/u16/u32/u64/u128`, `i8/i16/i32/i64/i128`,
  `f32`, `f64`, `bool`. Little-endian fixed-width format with
  per-type `Archive::validate`. Big-endian targets fail at compile
  time.
- Fixed-size composites: `ArchivedTuple2` … `ArchivedTuple8` and
  `Archive` impls for `(T0, T1)` … `(T0, …, T7)` and `[T; N]`, with
  `#[repr(C)]` layout, explicit zero padding, and `MaybeUninit`-
  driven safe Deserialize.
- Variable-length: `ArchivedBytes` and `ArchivedString` (lazy UTF-8
  validation on `as_str`); `Archive` impls for `Vec<u8>` and
  `String` with a `u32 LE` length prefix.
- Tagged unions: `ArchivedOption<T>` and `ArchivedResult<T, E>`,
  `#[repr(u32)]` discriminant + per-variant aligned payload; full
  `Archive` / `Serialize` / `Deserialize` impls for `Option<T>` and
  `Result<T, E>`.

---

## grpcx-idl-derive 1.0.0 — 2026-06-09

### Added
- `#[derive(GrpcxIdl)]` proc-macro — auto-generates `Archive` /
  `Serialize` / `Deserialize` impls for named-field structs and for
  enums with unit + single-payload tuple variants. Rejects lifetime
  generics, unions, unit / tuple structs, multi-payload tuple
  variants, and named-field variants with descriptive compile
  errors.

---

## grpcx-rt 1.0.0 — 2026-06-07

Async runtime, channels, and timers — own scheduler, no `tokio`.

### Added
- `Runtime`, `RtHandle`, `Handle`, `JoinHandle`, `Spawn`, `block_on`,
  `spawn`, `yield_now`.
- Multi-thread work-stealing runtime — per-worker lock-free Chase-Lev
  deques (LIFO local, FIFO steal), a `Mutex<VecDeque>` injector for
  overflow and off-worker spawns, and a dedicated reactor / timer
  driver thread unparked by `grpcx_io::Waker`. Cooperative budget +
  `yield_now` prevent worker starvation.
- Single-thread `current_thread` executor — `block_on` over
  `grpcx_io::Poll`; hand-rolled `RawWakerVTable` over `Arc<Task>`;
  `readable(fd)` reactor-readiness primitive.
- `channel`: bounded mpsc (`channel<T>(cap)`, `Sender` + `Receiver`
  with `try_send` / `try_recv` / async `recv`, `TrySendError` /
  `TryRecvError`); `oneshot::channel` with `Sender::send` and
  `Receiver: Future<Output = Result<T, Canceled>>`. Ring-backed; full
  → value returned, no allocation.
- Cross-thread unpark via `grpcx_io::Waker` (registered at a
  reserved token).
- Static-layer hierarchical timing wheel — 6 levels × 64 slots, O(1)
  insert, amortized-O(1) cascade firing. `Sleep` / `sleep`,
  `Timer::after`, `Timeout<F>` / `timeout` / `Elapsed`,
  `Interval` / `interval`.

### Fixed
- `channel::Chan::try_recv` TOCTOU race that could strand pushed
  values between the receiver's first `pop` returning `None` and its
  `senders.load(Acquire)`; receiver now re-pops after observing
  `senders == 0`.

### Changed
- Removed the public `Task` placeholder; tasks live in an internal
  slab cell, not on the public API.
- Removed the stall panic; futures awaiting a cross-thread wake now
  block on `poll(None)` instead of panicking on "no runnable task +
  no io + no timer".

---

## grpcx-io 1.4.0 — 2026-06-12

Cross-platform IO reactor (`epoll`, `kqueue`, `io_uring`) with zero
runtime dependencies.

### Added
- `IoErrorKind::ConnectionRefused` — Linux errno 111 / Darwin +
  FreeBSD 61. On connected UDP sockets this is the kernel surfacing
  ICMP port-unreachable.
- `udp_batch` module on Linux — `send_batch` / `recv_batch` over
  `sendmmsg` / `recvmmsg`. Connected non-blocking UDP sockets only,
  ≤ 64 datagrams per syscall, `WouldBlock` when no datagram moves.
- `udp_batch` module on Darwin / iOS — `send_batch` / `recv_batch`
  over the XNU batch-datagram syscalls `sendmsg_x` / `recvmsg_x`.
- `Waker` — `mio::Waker` parity. `Waker::new(&Registry, Token)`
  binds it to a reactor, `wake()` (callable from any thread) makes
  the bound `Poll::poll` return an event for that token, `reset()`
  re-arms. Linux uses `eventfd`; Darwin / iOS / FreeBSD use kqueue
  `EVFILT_USER`.
- Linux `io_uring` backend (sibling of `Poll`, not substitute) —
  `Ring::new(entries)` (with `BackendUnavailable` fallback on
  kernels < 5.1), `Ring::push_sqe`, `submit` / `submit_and_wait`,
  `reap_cqes`, `Sqe::nop` / `read` / `write`, `Cqe { user_data, res,
  flags }`. Detects single-mmap support at runtime.
- FreeBSD `kqueue` backend with the FreeBSD-shape `Kevent` struct
  (`Interest::PRIORITY` silently dropped — no `EVFILT_EXCEPT`).
- Darwin (macOS / iOS) `kqueue` backend with `EVFILT_READ` /
  `_WRITE` / `_EXCEPT` filters, `EV_CLEAR` edge-triggered semantics,
  `EV_RECEIPT` on every change, and the `timeout = NULL`
  fast-path.
- Linux `epoll` backend on x86_64 and aarch64 (`Poll::new` /
  `Poll::poll` / `Registry::{register,reregister,deregister}`),
  `SourceFd` wrapper for raw fds, `Events` container.
- Inline syscall layer via `core::arch::asm!` for every backend —
  no `libc` or `windows-sys` dependency. Carry-flag error helpers
  for Darwin and FreeBSD.
- Public API surface: `Poll`, `Registry`, `Events`, `Event`,
  `Token`, `Interest`, `Ready`, `IoError`, `IoErrorKind`,
  `IoResult`, `SourceFd`, plus the Linux-only `Ring` / `Sqe` /
  `Cqe`.

### Fixed
- FreeBSD release-mode SIGSEGV in `sys::freebsd::raw::syscall3` /
  `syscall6` — the kernel uses r8 / r9 / r10 as scratch and does
  not restore them; they are now declared clobbered.
- Darwin `register` initially omitted `EV_RECEIPT` from its change-
  list flags, which combined with the `timeout = NULL` fast-path
  blocked forever; `EV_RECEIPT` now everywhere.

### Performance
- `Events::poll_with` no longer pays a per-call `clear + resize`
  memset on the kernel-side scratch buffer; the buffer is reused
  across `poll()`s.
- kqueue `reregister` collapses two `kevent` syscalls (deregister
  then register) to one using a complement-DEL change list.
