# Changelog

All notable changes to `grpcx` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [0.25.0] below)

## [0.25.0] - 2026-06-12

**Passive idle-connection GC.**
`Server::set_max_connection_idle(Option<Duration>)` — a
serve-loop connection with no inbound traffic for the
window is reaped by its own drive task (TCP family +
multi-core h3 alike). PASSIVE: no probing — the
server-side-liveness-off conn-reuse-safety decision
stands; this bounds the per-idle-conn cost the multi-core
serves made material (a parked task waking ~200×/s plus a
registry entry, forever). Default `None` = unchanged.
The blocking client does NOT auto-reconnect: a reaped
client's next call fails and the caller re-establishes
(gRPC-conventional; tonic's channel reconnects itself).

## [0.24.0] - 2026-06-12

**ECONNREFUSED policy** (the design question recorded at
the h3-multicore checkpoint): a connected UDP socket's
ICMP port-unreachable now has defined semantics instead
of the opaque "udp: batch recv failed" —

- HANDSHAKING: fast-fail with the named
  `"udp: connection refused"` (TCP-connect parity —
  nobody is listening; no silent stall).
- ESTABLISHED: transient — a stray ICMP (server restart
  window, off-path noise) is swallowed; the liveness
  plane and the caller's deadline own the death verdict.
- Tests: mock-injected refusal on a live conn completes
  the call; dead-port call fails fast and named (Linux
  ICMP path verified on real sockets).

## [0.23.0] - 2026-06-12

**ws-mux over TLS.** The fourth transport's TLS form —
named at the wsmux-unified checkpoint, the `TlsTransport`
composition is now constructable:

- `ConnectedChannel::connect_ws_mux_tls(addr, tls,
  server_name, authority)` and
  `Server::listen_ws_mux_tls(_until)` — the WS upgrade +
  mux grammar riding TLS 1.3 exactly as the h2 form does.
- Gated on `all(transport-ws-mux, transport-h2-tls)`
  (both default) — no separate Cargo feature; lone-feature
  builds skip the combined surface.
- e2e: shapes (incl. a 64 KiB unary through the TLS
  record path — the crypto-1.3.1 regression class) +
  deadline verdict/reuse + untrusted-cert negative with
  no cleartext fallback.

## [0.22.0] - 2026-06-12

**Eager cancel on deadline expiry.** DEADLINE_EXCEEDED
was a purely local verdict — the server burned the whole
handler and shipped the response into a void. Now every
facade's deadline trip ALSO cancels the call on the wire
(best-effort flush, then the verdict; the connection
survives):

- h3: QUIC RESET_STREAM + STOP_SENDING (quic 1.9.0 /
  h3 1.8.0).
- h2: RST_STREAM(CANCEL) (h2 0.3.0).
- tcp-mux / ws-mux: new `MUX_RST` (0x06) frame — receiver
  drops per-sid state; cancelled-before-dispatch requests
  never run the handler.
- Gold: lockstep three-phase test (dispatch gated off
  while the cancel crosses; gate opens → handler count
  stays 0; same conn then serves a fresh call).

## [0.21.0] - 2026-06-12

**Multi-core `listen_h3`.** The flagship h3/UDP server
loop goes from single-threaded to router + task-per-conn
(the L2-Runtime serve pattern, grpcx 0.16.0, applied to
UDP):

- Caller thread = router: owns the socket, routes
  datagrams by peer address into per-conn grpcx-rt
  channels (bounded 256; a full inbox DROPS the datagram —
  UDP semantics, QUIC loss recovery owns it), parks on the
  socket fd when quiet.
- One task per connection on a `grpcx_rt::Runtime`
  (`drive_h3_conn`, mirroring `drive_tcp_conn` with the
  inbox in place of a per-conn fd); replies go straight
  out through a cloned socket. A slow handler no longer
  head-of-line blocks — and a broken conn no longer
  kills — every other connection (the pre-0.16 latent
  `?`-propagation existed here too).
- Shutdown: stop mirror + live-task counter + 2 s
  dead-man, verbatim from the TCP serve.
- Feature `transport-h3` now pulls `dep:grpcx-rt`.
- Measured: 8 clients × 5 ms handler — in-service
  high-water ≥ 2 (10/10 stable); idle 3 s server = 0.03 s
  CPU.

## [0.20.0] - 2026-06-12

**Linux UDP batch IO.** The `UdpTransport` batch overrides
(`send_batch` / `recv_batch`) widen from Darwin-only to
Darwin + Linux on grpcx-io 1.3.0's `sendmmsg`/`recvmmsg`
— the h3 lanes get the one-syscall-per-burst path on
Linux. No API change; `batch_buf` slots and semantics are
identical across platforms.

## [0.19.0] - 2026-06-13

**ws-mux unified — the fourth full transport.** ws-mux was
unary-only and in-memory-only; it now answers the same
four-RPC blocking API as h3 / h2 / tcp-mux, over real
sockets, with liveness (0.17) and deadlines (0.18) riding
along.

### Added
- `ConnectedChannel::connect_ws_mux(addr, authority)` —
  WS Upgrade over a real TCP socket. The RFC 6455 nonce +
  mask seed derive from wall clock + address hash —
  deliberately non-cryptographic per SPEC's masking note
  (masking defends proxy cache poisoning; TLS is the
  security boundary).
- `Server::listen_ws_mux(_until)` — the threaded
  L2-Runtime accept loop (`TcpServeConn` impl on
  `WsMuxServerConn`; pump/dispatch already matched).
- `call_streaming_ws_mux` core (the tcp-mux batch shape
  over masked WS binary frames) + ws-mux arms in
  `Channel::pump` / `is_ready` and all four blocking
  `call_*` dispatchers — previously these skipped ws-mux
  entirely.
- `transport-ws-mux` now pulls `grpcx-io` + `grpcx-rt`
  (socket + serve loop); the feature alone builds
  (matrix-checked).

### Acceptance
- `e2e_wsmux_socket.rs`: all four shapes byte-exact over
  real sockets + status propagation + a 50 ms deadline
  against a 300 ms handler (call verdict, connection
  survives), 10/10 repeat-stable. In-memory ws-mux suites
  unchanged-green.

## [0.18.0] - 2026-06-12

**Per-call deadlines** (plane 2 of the stall/deadline
split; plane 1 = 0.17.0's link liveness). The standard
`grpc-timeout` header, end to end, tonic-compatible.

### Added
- `CallContext::set_timeout(Duration)` — sets the
  spec-format `grpc-timeout` request header.
- `GrpcTimeout::from_duration` / `as_duration` — Duration
  conversions on the existing header codec (finest unit
  fitting the 8-digit cap).
- Client enforcement: every blocking facade (h3 unary /
  server-streaming / client-streaming / bidi, the h2 and
  tcp-mux streaming cores) parses the header from its
  request metadata and returns
  `Status(DEADLINE_EXCEEDED)` once the deadline passes —
  connection establishment included (gRPC semantics).
  Expiry is a CALL verdict: the connection stays usable
  (covered by e2e and by interop against tonic).
- Server honesty: a handler that returns after the peer's
  `grpc-timeout` already expired has its response replaced
  with DEADLINE_EXCEEDED (the client gave up; a stale
  success would be a lie). Synchronous handlers cannot be
  interrupted — the same constraint as any
  blocking-handler gRPC server.
- No default deadline (gRPC semantics: infinite unless
  set; 0.17.0's liveness plane bounds dead links).
- Interop gate grew a deadline case: a 500 ms tonic
  handler under a 100 ms grpcx-client deadline →
  DEADLINE_EXCEEDED near the deadline + post-deadline
  connection reuse, against real tonic.

## [0.17.0] - 2026-06-12

**Link liveness, atomized per transport** (user charter:
liveness belongs to the link; per-call time limits belong
to the caller — the deadline surface is the next
checkpoint). `IDLE_STALL` (~32 ms of client-side idle = a
"link stalled" error, which conflated a slow handler with
a dead link) is GONE; every transport now proves its link
with keepalive pings and declares death on a no-inbound
idle timeout.

### Added
- `Channel::set_link_liveness(keepalive, idle_timeout)` —
  dispatches to whatever transport the channel negotiated
  (h3 / h2 / tcp-mux / ws-mux); per-conn setters on the
  server conn types. Defaults 2.5 s / 10 s.
- mux grammar (tcp-mux + ws-mux): `MUX_PING (0x04)` /
  `MUX_PONG (0x05)` link-control frames on the reserved
  stream id 0.
- `ConnectedChannel::channel_mut()` for per-channel
  tuning.
- Slow handlers no longer kill clients: a ~100 ms
  sleeping handler under 8 concurrent clients completes
  (the scenario that surfaced the conflation).

### Changed
- `drive_once`'s idle counter only drives the spin/park
  policy; link death surfaces from the transport pumps
  as `Transport("…: link dead")`.
- Server-side conns ship with liveness OFF (the gRPC
  server convention — a blocking client doesn't pump
  between calls; probing/idle-killing healthy-but-quiet
  clients would break connection reuse). Opt in per conn
  for idle-conn GC.
- Client-side gap forgiveness: the first pump after an
  un-pumped gap (blocking caller between calls)
  re-anchors instead of declaring a false death;
  connection reuse across idle gaps is covered by e2e.

## [0.16.0] - 2026-06-12

**L2-Runtime TCP serving — event-driven, multi-core.** The
shared TCP accept loop (h2c / tcp-mux / h2-TLS servers)
was single-threaded and yield-spun when idle (~1 s CPU per
idle second). Now: the accept loop parks on the listener
fd (`FdParker`, 500 µs slices), and every accepted
connection runs as a task on a work-stealing
`grpcx_rt::Runtime` (workers = `available_parallelism`
clamped to [2, 8]), woken by `readable` on its own socket
with a 5 ms slice that bounds only stop latency. One slow
RPC no longer head-of-line blocks other connections, and
a poisoned connection ends its own task instead of
killing the whole server (pre-0.16 the loop `?`-propagated
per-conn pump errors).

### Changed
- `Service` trait objects are now `+ Send + Sync`
  (`Server::register(Box<dyn Service + Send + Sync>)`) —
  the tonic contract; connection tasks dispatch from
  worker threads. Typical impls satisfy it automatically.
- `Server` is `Clone` (cheap; shares the service registry
  — the tonic-`Router` shape). `register` while a serve
  loop holds the registry panics with a clear message;
  registration re-opens once every loop stops.
- `listen_*` signatures unchanged; `transport-h2` /
  `transport-tcp-mux` features now pull `grpcx-rt`.
- `listen_h3_until` (UDP, one socket) untouched — it
  already parks; splitting one socket across workers is a
  named, separate design.

### Acceptance
- New e2e (`e2e_threaded_serve.rs`): dispatch-overlap
  high-water mark ≥ 2 observed inside the service across
  8 concurrent clients (h2c + tcp-mux), register-while-
  serving panic contract, and an idle-CPU probe
  (`--ignored`): 3 s idle = 0.049 s CPU vs ~3 s under the
  old yield-spin (~60×).

## [0.15.0] - 2026-06-12

**L4 reliability phase 2 wired through.** Every H/3 pump
(client channel, server conn, the `listen_h3_until` loop)
now stamps a per-connection `Instant` epoch in µs into
`grpcx_h3::Connection::pump_at`, so the QUIC layer's RTT /
PTO / handshake-retransmit timers actually run. A lost
flight TAIL, a lost ACK, or a lost handshake flight is
recovered by a probe timer (~1-10 ms) instead of surfacing
as the ~32 ms "link stalled" transport error. The 500 µs
reactor park slice already bounds timer latency, so the
drive-loop policy is unchanged.

### Added
- e2e gold test: `call_unary` over an in-memory transport
  that eats flight tails succeeds via PTO recovery.

## [0.14.2] - 2026-06-12

**Large-datagram-ready batch slots (perf patch, no API
change)** — the `net.inet.udp.maxdgram` experiment's
shipped half.

### Changed
- `UdpTransport` Darwin batch layout: 32 × 16 KiB →
  **8 × 64 KiB** slots (same 512 KiB footprint). A raised
  `maxdgram` sysctl can never truncate a batch receive,
  and width 8 still covers a full 64 KiB burst at the
  default sysctl in one syscall.

### Measured (the experiment, M4 Max, within-run)
- Default sysctl (9 216 ⇒ 8 packets): 64 KiB 1.51-1.53×
  behind tonic.
- `maxdgram=65535` (⇒ 65 000 budget, 2 data packets):
  **111-113 µs vs tonic ~90 — 1.26×**. Per-call ledger:
  send 27 → 10.2 µs, recv 10 → 4.4 µs (IO -60 %); the
  dominant remaining term is the peer-processing wait
  (~55 µs park, which now carries the two-way kernel
  wake tax ~20 µs). Spin 48 re-tested at the new shape:
  still WORSE than 4 on every lane — the wake tax can't
  be bought back by burning empty recvs.
- Deploy guidance recorded in BUDGETS: large-payload
  loopback deployments should raise `maxdgram`; the
  comparator bins now probe the live sysctl
  (`src/budget.rs`) instead of hardcoding 9 000.

## [0.14.1] - 2026-06-11

**Stack pass 2 + the spin-budget ledger (perf patch, no
API change).** Rides grpcx-quic/grpcx-h3 1.4.1.

### Results (steady-state, M4 Max, within-run vs tonic)
- 16 B **32.3 µs vs 42.0 (-23 %)**, 1 KiB **33.2 vs 53.6
  (-38 %)**, 64 KiB 134 vs 88.7 (**1.51×**, was 1.65×).
- Pure stack 64 KiB: 81.7 → **~59-68 µs** (-16 % on the
  profiling harness); remaining profile is ~20 % AEAD
  (the encryption floor) + fragmented 1-2 % copies at
  semantic boundaries.
- 64 KiB UDP RPS 1 470 (v0.11) → **~7 400** (5×).

### Changed
- Drive-loop spin budget 64 → **4**: a per-syscall ledger
  (profileudp's `CountingTransport`) showed 66.8 empty
  recv syscalls ≈ 41 µs per 64 KiB call; early parking
  wins every lane (16 B -31 %, A/B'd on a quiet machine —
  the earlier "16 is worse" reading was load noise).
- `write_message_frame`: gRPC message header + payload go
  down as two `write_data` calls — the payload is no
  longer staged through an intermediate `encode_frame`
  Vec (one full-message copy gone; lands as two H/3 DATA
  frames, legal segmentation).

### Erratum
- v0.13/v0.14 docs named "ACK coalescing" as the next
  64 KiB lever. Misrecord: L4 carries no ack machinery at
  all (lockstep-loopback semantics) — there are no ACKs
  to coalesce. Corrected in both REFACTOR docs; real
  reliability (loss detection + retransmit + acks) is an
  unscoped correctness feature, called out to the user.

## [0.14.0] - 2026-06-11

**The reactor park pass — quiet links sleep in the kernel
(L1 integration).** Every socket-backed drive loop used to
yield-spin when its link went idle: 100 % of a core per
idle connection, and the wake depended on scheduler luck.
Now the loop parks in L1's reactor (`kqueue` on
Darwin/FreeBSD, `epoll` on Linux, via `grpcx_io::Poll`)
and the kernel wakes it the moment a datagram lands.

### Results (steady-state, M4 Max, A/B same machine)
- **Idle server CPU: 2.88 s → 0.02 s per 3 s idle
  (~144×)** — `listen_h3_until` with no traffic burned
  ~86 % of a core under the yield policy; parked it is
  ~0.6 %.
- Lane latency: park ≈ yield within run-to-run noise on
  every payload (A/B alternated binaries) — the serial
  loopback lanes spend their idle cycles inside the
  64-cycle spin budget, so the park rarely engages
  mid-call. Earlier (16) and later (128) park thresholds
  both measured WORSE.
- This falsifies half of 0.13.0's closing hypothesis: the
  remaining 64 KiB UDP-lane gap vs tonic (~146 µs vs
  ~89 µs this run; 16 B and 1 KiB lanes still WIN by
  25-38 %) is NOT the idle-wait shape. It is per-datagram
  IO + in-stack cost — owned by stack pass 2 next. Pure
  stack stays below tonic's whole cleartext round trip
  (~72 vs ~89 µs).

### Added
- `DatagramTransport::wait_readable(timeout) ->
  Result<bool, RpcError>` — park until the link may be
  readable. Default: yield once, return `false` (queue
  pairs have no OS handle). `UdpTransport`,
  `TcpTransport` override with a lazily-built one-fd
  reactor (`FdParker`, edge-safe: loops drain to
  `WouldBlock` before parking); `TlsTransport` forwards
  to its inner TCP.

### Changed
- `drive_once` idle policy: 64-cycle spin, then park (one
  500 µs slice per cycle, accounted 1024 idle ticks — a
  dead link still reports "link stalled" within ~32 ms).
- `Server::listen_h3_until`: parks on the listen socket
  when no connection made progress (bounds stop-flag
  latency to ~500 µs).
- `transport-h2` / `transport-tcp-mux` features now pull
  `grpcx-io` (target-gated to platforms with a wired
  selector; others keep the yield default).

### Out of scope (named)
- `serve_tcp_until` (multi-conn TCP accept loop) still
  yields — its reactor-ization needs a multi-fd
  registration surface and lands with the L2
  multithread-Runtime serve item.

## [0.13.0] - 2026-06-11

**The datagram-budget pass — the 64 KiB lane's biggest
single jump.** A 64 KiB transfer was ~60 MTU-sized
datagrams at the old fixed 1100-byte budget; per-packet
sealing, allocation, ACKs, and scheduling all scaled with
the count. Rides grpcx-quic/grpcx-h3 1.4.0.

### Results (steady-state, M4 Max, same runs)
- 64 KiB UDP lane @9000 budget: 382 → **~166 µs / 5.8k
  RPS** — cumulative from the 685 µs v0.11 baseline:
  **-76 %**, RPS 1 470 → 5 800.
- Pure-stack lane @9000: **~85 µs** — BELOW tonic's whole
  round trip (~90-98 µs; note tonic's lane is cleartext
  TCP while grpcx is fully-encrypted QUIC). The remaining
  UDP-lane gap (~80 µs) is IO-drive shape (two-thread
  spin/yield vs tokio's event wakeups) — the multithread
  reactor item on the v1.x menu.

### Added
- `Channel::set_datagram_budget` /
  `ServerConn::set_datagram_budget` — passthrough to L4.
  Default budget raised 1100 → 1350 (internet-safe);
  loopback callers set ~9000 (Darwin maxdgram 9216).

### Changed
- `UdpTransport`'s Darwin batch-receive slots: 2048 B
  stack array per call → reused heap buffer with 16 KiB
  slots (large-datagram ready; no per-call zeroing).
- Spin-then-yield idle threshold re-validated at 64
  (512 measured worse: spinning recv syscalls contend
  with the peer thread).

## [0.12.0] - 2026-06-11

**UDP batch IO — the 64 KiB lane gets its first cut.**
Per-MTU syscalls dominated bulk transfers on `grpcx/h3`
(a 64 KiB response is ~60 datagrams, one `sendto`/`recv`
each). Rides grpcx-io 1.2.0's Darwin batch-datagram
syscalls (`sendmsg_x` / `recvmsg_x`, ≤64 datagrams per
syscall).

### Results (steady-state, Apple M4 Max, 2026-06-11, same run)
- 64 KiB: 684.9 µs p50 / 1 470 RPS → **456.7 µs / 2 130
  RPS** (+45 % RPS).
- 16 B: 40.5 → 37.6 µs; 1 KiB: 54.2 → 45.8 µs.
- Still behind tonic at 64 KiB (125 µs): the remaining
  gap is now IN-STACK (the pure-protocol mem lane runs
  ~229 µs by itself) — the next cut is a profile pass
  over the 64 KiB path (copy / crypto batching), not IO.

### Added
- `DatagramTransport::send_batch` / `recv_batch` —
  default implementations loop over `send`/`recv`
  (existing custom transports keep working);
  implementations with a kernel batch path override.
- `UdpTransport` upgrades itself to an OS-connected
  socket (client: at `connect_h3`; server: when the
  first sender is locked) — connected sockets unlock the
  Darwin batch syscalls. Falls back to the single-datagram
  path while unconnected; non-Darwin platforms keep the
  loop fallbacks.

### Changed
- `drive_once` + the comparator server loop move bursts
  with `send_batch`/`recv_batch`: replies accumulate
  across a received burst and leave in one syscall.

## [0.11.0] - 2026-06-11

**h2 over TLS (T4) — the T-flow closes.** The same H/2
session as h2c with L3's byte-stream TLS 1.3 as transport
middleware; the sans-IO h2 connection never knows TLS is
there. Rides a grpcx-crypto 1.3.0 layer-up patch
(`ClientHandshake::is_complete` /
`ServerHandshake::is_complete` — the network-driven
handshake loop needs a non-consuming completion query).

### Added
- `transport-h2-tls` feature (default; implies
  `transport-h2`, pulls `grpcx-crypto`) —
  `TransportKind::H2`, `Channel::connect_h2_session`,
  `ConnectedChannel::connect_h2(addr, tls, server_name,
  authority)`, `Server::listen_h2` / `listen_h2_until`,
  `TlsTransport`.
- `TlsTransport` — `DatagramTransport` middleware over
  non-blocking TCP. The handshake advances lazily through
  `send`/`recv` (no accept-loop blocking); app data
  queued mid-handshake flushes on promotion; plaintext a
  peer pipelines behind its Finished is drained into an
  inbox.
- e2e `tests/e2e_h2_tls.rs` — typed stubs through a real
  TLS 1.3 handshake (unary + server-streaming +
  trailers-only error), plus the untrusted-cert negative
  path (no silent cleartext fallback).

### Changed
- The shared TCP accept loop is generic over the
  per-connection transport (`serve_tcp_until_with`) — h2c
  / tcp-mux take bare TCP, `listen_h2` wraps each
  accepted stream in `TlsTransport`.

### Notes
- ALPN is caller-configured (`TlsConfig::set_alpn` —
  gRPC servers expect `h2`); `connect_h2` does not force
  it.
- TLS interop against tonic (`tonic` TLS endpoints) is
  not in the gate yet — the interop gate runs h2c; TLS
  form is loopback-verified.

## [0.10.0] - 2026-06-11

**Streaming facades on every transport.** All four
[`RpcKind`]s now flow through every wired transport —
`grpcx/h3`, h2c, and `grpcx/tcp-mux` — behind the same
generated stubs and `ConnectedChannel` methods. **The
tonic interop gate now also covers streaming**:
server-streaming + bidi verified live in both directions
over h2c (`comparators/tonic-grpcx`, `interop` bin).

### Added
- h2c streaming core (`call_streaming_h2`): batch shape —
  HEADERS + one gRPC frame per request message +
  half-close, every response message collected to the
  trailers.
- tcp-mux streaming core (`call_streaming_tcp_mux`) over
  the shared mux layer (`poll_completed_mux`), one DATA
  record per message, END_STREAM on the last.
- Interop gate grew `Burst` (server-streaming) and
  `Exchange` (bidi) rpcs, asserted in both directions.

### Changed
- `call_server_streaming` / `call_client_streaming` /
  `call_bidi` free fns moved from the H/3 module to the
  shared transport seam and now dispatch on the channel's
  negotiated transport (same re-export paths; previously
  H/3-only). `ConnectedChannel`'s streaming methods moved
  with them — available under any transport feature.
- Unary facades are now thin wrappers over the streaming
  cores on h2c / tcp-mux (one code path per transport).

### Notes
- Batch shape only (all requests out, then all responses
  collected) — incremental per-message streaming is the
  v1.x backpressure surface, matching the H/3 facades.
- ws-mux streaming stays unary-only pending its unified
  API pass (v1.x menu) — the shared mux layer already
  supports the shapes.

## [0.9.0] - 2026-06-11

**`grpcx/tcp-mux` — the bare-metal transport (T7).** The
v0.6 ws-mux shed of its WebSocket shell: same mux frame
grammar, `u32-BE length || mux-frame` records over raw
TCP. No TLS, no HTTP, no handshake (prior knowledge, like
h2c), zero added deps. See SPEC.md § grpcx/tcp-mux.

### Added
- `transport-tcp-mux` feature (default) —
  `TransportKind::TcpMux`,
  `Channel::connect_tcp_mux_session`,
  `ConnectedChannel::connect_tcp_mux(addr, authority)`,
  `Server::listen_tcp_mux` / `listen_tcp_mux_until`,
  `TcpMuxServerConn`.
- e2e `tests/e2e_tcpmux.rs` — typed stubs over real TCP
  (echo + reuse + trailers-only error + 512 KiB payload
  crossing record/read-chunk boundaries + record-cap
  rejection).

### Changed
- Mux layer extracted to `mux.rs` (frame grammar,
  header-list codec, stream routing, unary/dispatch
  logic), shared by ws-mux and tcp-mux; `wsmux.rs` is now
  the WebSocket shell only. Public re-export paths for
  `MUX_*` / `encode_mux_frame` / `decode_mux_frame` /
  `encode_header_list` / `decode_header_list` unchanged
  (now also available under `transport-tcp-mux` alone).
  Mux codec error strings now say `mux:` instead of
  `ws-mux:` (`&'static str` payloads, not part of the
  typed API).
- `TcpTransport` moved to the shared transport seam
  (re-export path unchanged); the h2c and tcp-mux accept
  loops now share one generic TCP serve loop.

### Fixed
- `TcpTransport::send` used `write_all` on the
  non-blocking socket — a chunk larger than the free
  kernel send buffer failed mid-write with `WouldBlock`
  (h2c never tripped it: H/2 flow control caps in-flight
  data at 64 KiB; tcp-mux sends whole records).
  Now a write loop that spin-yields on `WouldBlock` with
  a stall cap for dead peers.

## [0.8.0] - 2026-06-11

**The DX pass + the H2 interop bridge.** Two work streams
since v10.0: the tonic-habit usability pass
([docs/dx-scope.md](../../docs/dx-scope.md) D1-D6) and the
h2c transport chartered as the interop bridge to existing
gRPC deployments
([REFACTOR-v10-v0.8-h2-bridge.md](../../REFACTOR-v10-v0.8-h2-bridge.md)).
**Gate delivered: live bidirectional interop with tonic
over h2c** — grpcx client ↔ tonic server and tonic client
↔ grpcx server, 16 B / 1 KiB / 64 KiB echo + error-status
propagation (`comparators/tonic-grpcx`, `interop` bin).

### Added — DX pass (D1-D6, docs/dx-scope.md)
- `CallContext` — symmetric per-call context (request
  metadata out, trailing metadata back) replacing tonic's
  `Request<T>`/`Response<T>` wrapper noise; generated
  handlers take `(message, &mut CallContext)`.
- `ConnectedChannel` — channel + transport bundled; the
  tonic-habit client handle generated stubs take
  (`connect_h3(addr, tls, server_name)` + blocking
  `call_*` methods).
- `Server::listen_h3` / `listen_h3_until` — the
  `serve(addr)` shape: single-threaded multi-connection
  UDP accept loop.
- Typed service stubs from L8 codegen (`XxxClient` /
  `XxxHandler` / `XxxServer`) + `proto!` / `proto_inline!`
  macros (grpcx-pb-derive): `.proto` → typed API with no
  protoc, no build.rs. Consumer dep surface: `grpcx` +
  `grpcx-pb`.
- proto3 dialect front-end in grpcx-pb (editions-2023
  core; proto3 translated at the boundary).
- `docs/MIGRATING-FROM-TONIC.md`.

### Added — H2 interop bridge (charter 2026-06-11)
- `grpcx-h2` crate (L5 sibling): sans-IO HTTP/2
  connection state machine (RFC 9113 subset: preface /
  SETTINGS / two-level flow control with WINDOW_UPDATE
  replenishment / HEADERS+CONTINUATION / PADDED+PRIORITY
  stripping / soft-close stream reap) over HPACK
  (RFC 7541: static+dynamic tables, Huffman decode,
  RFC appendix-C KAT coverage).
- `transport-h2` feature (default) — `TransportKind::H2c`,
  `Channel::connect_h2c_session`,
  `ConnectedChannel::connect_h2c(addr, authority)`,
  `Server::listen_h2c` / `listen_h2c_until`,
  `H2ServerConn`, `TcpTransport` (non-blocking std TCP).
  h2c = cleartext prior-knowledge HTTP/2: zero TLS setup,
  wire-compatible with tonic `http://` endpoints.
- e2e `tests/e2e_h2c.rs` — typed stubs over real TCP
  loopback (echo + session reuse + trailers-only error).

### Changed
- Transport seam extracted to `transport_common`
  (`DatagramTransport` / `ConnectedChannel` / blocking
  `call_unary` dispatcher) — every feature combination of
  {`transport-h3`, `transport-ws-mux`, `transport-h2`}
  now builds standalone. Public re-export paths
  unchanged.
- `Channel::pump` / `is_ready` dispatch on the negotiated
  transport (h3 / h2c).

### Fixed
- grpcx-h2: trailer HEADERS emitted ahead of
  window-buffered DATA (DATA-after-END_STREAM to a real
  peer — our lenient loopback decoder masked it; tonic
  rejects it). Trailers now queue behind the data and
  carry END_STREAM themselves; regression-locked by frame
  ordering KAT + the live tonic gate.

### Notes
- Streaming RPCs over h2c (`call_server_streaming` etc.)
  are not wired yet — unary first per the T5a plan; the
  h2 connection layer already supports the shapes.
- grpcx-h2 stays at 0.1.0 until the workspace publish
  pass (path-dep only; never published).

## [0.7.0] - 2026-06-10

**Stabilisation + the GATING e2e comparison vs tonic** —
work unit 7. The charter-promoted acceptance item
delivered: grpcx beats tonic end-to-end on small/medium
unary payloads over real OS sockets. See BUDGETS.md
§ "e2e vs tonic" and
[../../REFACTOR-v10-v0.7-stabilisation.md](../../REFACTOR-v10-v0.7-stabilisation.md).

### Added
- `UdpTransport` — std-UDP `DatagramTransport`
  (non-blocking; server side locks onto the first peer).
  First real-socket seam; the L1/L2 reactor integration is
  v1.x.
- `comparators/tonic-grpcx` (workspace member,
  publish = false): three-lane harness — grpcx-h3 over
  UDP loopback, grpcx-h3 over in-memory loopback (pure
  protocol-stack cost), tonic 0.12 over TCP loopback.
  tonic/prost/protox enter the workspace as bench-only
  exceptions.
- e2e test `blocking_call_unary_over_real_udp_loopback`.

### Results (steady-state, Apple M4 Max, 2026-06-10)
- 16 B: **23.7 µs p50 / 35.6k RPS** vs tonic 33.5 µs /
  29.3k — grpcx wins.
- 1 KiB: **34.2 µs / 29.1k** vs 40.7 µs / 23.8k — grpcx
  wins.
- 64 KiB: 505 µs vs 69 µs — tonic wins; gap is IO-layer
  (per-MTU sendto syscalls; pure-stack lane does 64 KiB in
  202 µs). Batched UDP syscalls are the v1.x L1 work.

### Fixed (found by the harness — the gating item's value)
- Whole-stack O(calls²): every RPC leaked one dead stream
  entry in L4+L5; fixed via `close_request`/`close_stream`
  (grpcx-quic 1.3.0 + grpcx-h3 1.3.0) wired into all call
  completion paths + server dispatch.
- `call_unary` drive loop serialized one datagram per
  scheduling round; now batch-drains the receive queue and
  busy-spins briefly before yielding.

## [0.6.0] - 2026-06-10

**`grpcx/ws-mux` fallback transport + ALPN negotiation** —
work unit 6. Acceptance met: round-trip unary over
ws-mux, including two concurrent calls multiplexed on one
WS connection. See
[../../REFACTOR-v10-v0.6-ws-mux.md](../../REFACTOR-v10-v0.6-ws-mux.md).

### Added
- `transport-ws-mux` cargo feature (default) — pulls only
  L9 (`grpcx-ws` 1.1); pure `no_std + alloc`.
- ws-mux protocol locked in SPEC.md: one mux frame per WS
  binary message (`u32 stream-id | u8 type|flags |
  payload`; HEADERS/DATA/TRAILERS; 0x80 = END_STREAM;
  odd client stream ids; stateless u16-BE header-list
  serialization — no QPACK on the fallback path).
- Mux codec pure functions: `encode_mux_frame` /
  `decode_mux_frame` / `encode_header_list` /
  `decode_header_list` + `MUX_*` constants.
- `Channel::connect_ws_mux` (caller supplies the RFC 6455
  nonce + a mask seed — no RNG in the crate; SPEC
  masking note) / `pump_ws` / `is_ready_ws` /
  `begin_unary_ws` / `poll_unary_ws` + `WsUnaryCall`.
- `Server::accept_ws_mux` → `WsMuxServerConn`
  (`pump` / `dispatch`), full WS upgrade handshake via
  L9, trailers-only error responses, dispatch through
  the same `dispatch_streaming` surface as H/3.
- ALPN + Alt-Svc pure functions (feature-independent):
  `select_alpn_transport` (h3 preferred, ws-mux fallback,
  no HTTP/2), `build_alt_svc_header` /
  `parse_alt_svc_header`.
- `tests/e2e_wsmux.rs` — unary echo, error-status
  propagation (UTF-8 message), concurrent multiplexed
  calls, ALPN selection, Alt-Svc round-trip, mux codec
  KATs.

### Notes
- Streaming over ws-mux: the frame grammar already
  carries it; the client call surface lands with the
  v0.7 transport-unified API pass.
- ws-mux over TLS/TCP sockets: the byte-stream pump is
  the seam; the socket runtime is v0.7+ work.

## [0.5.0] - 2026-06-10

**Bidirectional streaming + typed `grpc-timeout`** — work
unit 5. Acceptance met: bidi round-trip KATs. See
[../../REFACTOR-v10-v0.5-bidi.md](../../REFACTOR-v10-v0.5-bidi.md).

### Added
- `Channel::begin_bidi` / `Channel::send_bidi_message` /
  `Channel::finish_bidi_sending` + `BidiCall` (a type
  alias of `ServerStreamingCall` — the receive side IS
  the server-streaming shape; composition over new state).
- `GrpcTimeout` + `TimeoutUnit` (gRPC spec "Timeout"
  wire format: ≤ 8 digits + `H/M/S/m/u/n`): `to_header` /
  `parse` / `from_metadata`, carried per-call through
  request `Metadata`. Deadline *enforcement* lands with
  the IO runtime (v0.6+) — this release owns the format.
- `tests/e2e_streaming_h3.rs` + 2: bidi sum (3 up → 1
  down + Complete), bidi count (1 up → 3 down).
- `tests/kat_wire.rs` + 3: timeout format vectors (all
  six units, 8-digit max), parse round-trip + rejects,
  metadata travel through builder + parser.

## [0.4.0] - 2026-06-10

**Server-streaming + client-streaming over `grpcx/h3`** —
work unit 4. Acceptance met: round-trip KATs for both
shapes on the loopback harness. See
[../../REFACTOR-v10-v0.4-streaming.md](../../REFACTOR-v10-v0.4-streaming.md).

### Added
- `Service::dispatch_streaming` (default method —
  additive): full request-message sequence in, response
  sequence out; the default bridges to `dispatch`
  (1-in/1-out), so existing unary services are untouched.
  The transport now routes ALL RPCs through this surface.
- `framing::try_decode_frame` — incremental decode for
  streaming bodies (`Ok(None)` on partial input;
  structural errors still reject).
- `StreamEvent` (`Message` / `Complete(Metadata)`).
- `Channel::begin_server_streaming` /
  `Channel::poll_stream_message` + `ServerStreamingCall`.
- `Channel::begin_client_streaming` /
  `Channel::send_stream_message` /
  `Channel::finish_sending` + `ClientStreamingCall`
  (a type alias of `UnaryCall` — collecting the single
  response IS the unary wait, via `poll_unary`).
- `tests/e2e_streaming_h3.rs` — count down-stream (5
  messages + trailing metadata), empty stream completes,
  mid-stream error propagation, sum up-stream,
  unary-against-streaming-service.
- `tests/kat_wire.rs` + 2: `try_decode_frame` pending on
  every strict prefix / hard error on bad flag.

### Fixed
- All `write_data` call sites now go through a
  `write_all` helper: L5 returns the accepted byte count
  (QUIC flow control may take less), and a short write
  now raises `Transport("partial write")` instead of
  silently truncating the gRPC frame stream. Buffered
  retry on flow-control credit is v1.x.

### Changed
- `ServerConn::dispatch` frames each response message
  separately (M frames for M messages).

## [0.3.0] - 2026-06-10

**Unary RPC over `grpcx/h3`** — work unit 3. First
transport edge: L3 (TLS) + L4 (QUIC) + L5 (HTTP/3) wired
behind the new `transport-h3` feature (default-on).
Acceptance met: local loopback unary RPC succeeds — both
the single-thread sans-IO drive and the two-thread
blocking facade. See
[../../REFACTOR-v10-v0.3-unary-h3.md](../../REFACTOR-v10-v0.3-unary-h3.md).

### Added
- `transport-h3` cargo feature (default) — pulls
  `grpcx-crypto` / `grpcx-quic` / `grpcx-h3 1.3` (the
  trailing-HEADERS layer-up minor) and std. Without it
  the crate stays `no_std + alloc` (ADTs + wire codecs
  only).
- `DatagramTransport` trait — the IO seam the blocking
  facade drives (in-memory pairs in tests; UDP sockets
  arrive with the L1/L2 runtime at v0.6+).
- `Channel::connect_h3` / `Channel::pump` /
  `Channel::is_ready` — sans-IO client session.
- `Channel::begin_unary` / `Channel::poll_unary` +
  `UnaryCall` — non-blocking unary primitives. Handles
  the trailers-only error fast path.
- `call_unary(channel, transport, method, metadata,
  request)` — blocking facade; drives handshake + call to
  completion, stall-guarded.
- `Server::accept_h3` + `ServerConn::pump` /
  `ServerConn::dispatch` — server session + request
  routing into registered `Service`s, with
  trailers-only error responses (bad request /
  unknown service / unimplemented compression / service
  failure).
- `tests/e2e_unary_h3.rs` — echo round-trip,
  error-status propagation, unknown-service →
  `Unimplemented`, blocking two-thread `call_unary`.

### Changed
- **Pre-1.0 break**: free function `call_unary` gains the
  `transport: &mut dyn DatagramTransport` parameter and
  takes `&mut Channel` (was `&Channel` + unimplemented).
- `Channel::connect` stays `unimplemented!()` — its
  socket-backed semantics arrive with the IO runtime; the
  sans-IO `connect_h3` constructor is the live path.
- Crate is `no_std` only without `transport-h3` now
  (`cfg_attr`-gated).

## [0.2.0] - 2026-06-10

**gRPC wire framing** — work unit 2. Length-prefixed
message codec + HTTP/3-shaped header/trailer mapping, per
[SPEC.md](./SPEC.md). 22 wire KATs including the gRPC-spec
unary frame vector (byte-identical with tonic for the
same payload). See
[../../REFACTOR-v10-v0.2-wire-framing.md](../../REFACTOR-v10-v0.2-wire-framing.md).

### Added
- `src/framing.rs` — `MessageFrame` + `encode_frame` /
  `decode_frame` / `decode_all_frames` +
  `FRAME_HEADER_LEN`. Pure byte codec; per-call policy
  (max sizes, compression negotiation) is the v0.3 call
  layer's job.
- `src/headers.rs` — `encode_request_headers` /
  `parse_request_headers` / `encode_response_headers` /
  `encode_trailers` / `parse_trailers` +
  `percent_encode` / `percent_decode_lossy` (lenient, per
  spec) + `CONTENT_TYPE_GRPC` / `USER_AGENT`.
- `StatusCode::from_i32` — numeric `grpc-status` → enum
  (`None` outside `0..=16`).
- `tests/kat_wire.rs` — 22 KATs: framing round-trips +
  reject paths (truncated header/payload, bad flag,
  trailing garbage), request-header round-trip + reject
  paths (non-POST, bad content-type, malformed `:path`
  × 5), trailer round-trips (Ok / error+message+metadata
  / two-digit code / reject paths), percent-coding
  vectors (UTF-8, stray `%`, lowercase hex, all-bytes),
  and a full unary wire walk-through.
- `SPEC.md` § grpc-message percent-encoding rules.

### Changed
- Nothing on the v0.1 API surface — additive only.

## [0.1.0] - 2026-06-10

**API skeleton + design docs.** First L10 checkpoint. The
public surface is frozen and each item carries a doc
comment, but bodies are `unimplemented!()`. Implementation
lands across v0.2-v0.7; see
[../../REFACTOR-v10-v0.1-bootstrap.md](../../REFACTOR-v10-v0.1-bootstrap.md).

### Added
- `Service` — trait for server-side method dispatch.
  Takes `(method_name, metadata, request_bytes)` and
  returns `(response_bytes, response_metadata)` or an
  `RpcError`. Trait-object dispatch lets `Server::register`
  hold heterogeneous services.
- `Method { service, method, kind: RpcKind }` — per-RPC
  metadata.
- `RpcKind` — `Unary` / `ServerStreaming` /
  `ClientStreaming` / `BidirectionalStreaming`. Covers
  every gRPC RPC shape.
- `Status { code, message }` — gRPC status pair.
- `StatusCode` — every gRPC standard code 0..=16
  (`Ok` / `Cancelled` / `Unknown` / `InvalidArgument` /
  `DeadlineExceeded` / `NotFound` / `AlreadyExists` /
  `PermissionDenied` / `ResourceExhausted` /
  `FailedPrecondition` / `Aborted` / `OutOfRange` /
  `Unimplemented` / `Internal` / `Unavailable` /
  `DataLoss` / `Unauthenticated`).
- `Metadata { headers: Vec<(String, Vec<u8>)> }` —
  arbitrary HTTP-shaped headers carried alongside an RPC.
- `RpcError` — failure modes:
  `Status(Status)` (RPC-level failure with code) /
  `Transport(&'static str)` (transport-layer failure) /
  `Codec(&'static str)` (serialisation failure) /
  `TransportUnavailable` (no supported transport).
- `TransportKind { H3, WsMux, WebTransport }` —
  enumerates the supported transports.
- ALPN constants: `ALPN_H3: &str = "grpcx/h3"`,
  `ALPN_WS_MUX: &str = "grpcx/ws-mux"`.
- `Channel` — client-side handle with stubs for
  `connect(addr, transport)` and `transport()`.
- `Server` — server-side dispatcher with stubs for
  `new()`, `register(Box<dyn Service>)`, and
  `listen(addr, transport)`.
- `call_unary(channel, method, metadata, request)` —
  free-function entry-point stub for the client side of
  a unary RPC.
- `Cargo.toml` — full published metadata; `version =
  0.1.0` (literal); no runtime deps (L10 v0.1 is API-only;
  transport deps land at v0.3+).
- `README.md`, `BUDGETS.md`, `SPEC.md`, this `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx` as the
  eleventh member.

### Gate
- `cargo test --workspace`: workspace builds clean. L10
  ships 2 smoke tests (`error_is_compact`,
  `status_code_numerics_match_spec`); workspace total
  **777 / 0** (was 775 at L9 v9.0; +2).
- `cargo clippy --workspace --all-targets -- -D warnings`:
  clean.
- `cargo doc --workspace --no-deps`: zero-warning across
  the `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check --all`: clean.
