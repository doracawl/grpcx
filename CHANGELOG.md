# Changelog

All notable changes to the `grpcx` project are documented in this file.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`grpcx` is a from-scratch RPC stack built as ten separately versioned
crates, layered from the OS reactor up to the gRPC façade. Each crate
below has its own H2 release section; entries under one crate are
ordered newest-first.

---

## grpcx — Unreleased (internal-RPC v1)

The internal-RPC v1 stack — three new same-host / same-network
transports carrying wire format v2, plus a `RpcChannel` trait that
lets generated client stubs ride either the existing v1 mux-frame
pipelines or the v2 wire pipelines transparently. Binding plan:
`_dev/IMPLEMENTATION-PLAN.md`; architecture: `_dev/RFC-internal-rpc-arch.md`;
wire normative: `_dev/SPEC-wire-v2.md`.

### Added

- **`transport-uds`** feature — AF_UNIX same-host transport.
  - `UdsConnection::connect(path, authority)` + the four
    `call_unary` / `call_server_streaming` /
    `call_client_streaming` / `call_bidi` methods.
  - `Server::listen_uds[_until](path[, stop])`.
  - `UdsServerConn` reserved as a future sans-IO surface.
  - Kernel-attested `PeerCred` (uid/gid/pid via `SO_PEERCRED` /
    `getpeereid(3)`) surfaced through `CallContext::peer_cred()`.
- **`transport-shmem`** feature (depends on `transport-uds`) —
  POSIX shared-memory ring transport with UDS rendezvous control
  plane + wake fds via `SCM_RIGHTS`.
  - `ShmemConnection::connect(rendezvous_path, authority)` + the
    four call shapes.
  - `Server::listen_shmem[_until]`.
  - `ShmemConnection::set_busy_poll_threshold(Duration)` for the
    sub-microsecond busy-poll mode (vs the default park-on-wake-fd).
  - `ShmemConnection::set_busy_poll_unbounded(bool)` +
    `Server::listen_shmem_busy_poll_until(path, stop)` — symmetric
    **both-or-neither** mode where both ends drop wake-fd syscalls
    entirely (consumer `spin_loop` on the ring head, producer
    publishes without poking the wake pipe). Closes the per-frame
    `pipe(2)` syscall that bottlenecked throughput-class flows.
    lx64 shmem-throughput 0.093 → 1.49 GB/s (16×); macOS arm64
    > 5 GB/s (RFC § 8 gate cleared on this hardware).
  - `SLOTS_PER_RING` bumped from 4 → 16 (Rust + the TS N-API mirror
    `kSlotsPerRing` stays in sync); region now 128 KiB per side
    instead of 16 KiB. Gives the busy-poll producer enough headroom
    to stop spinning on `Full` every fourth push.
  - **v1.5 — Rigtorp cached opposite-side index** on the SPSC ring:
    `Ring::push_cached(header, payload, &mut cached_rpos)` and
    `Ring::pop_cached(out, &mut cached_wpos)`. Producer reads its own
    local cached `reader_pos` snapshot on the hot path; only on a
    "cache says Full" miss does it pay an `Acquire`-load of the true
    `reader_pos`. Consumer side symmetric. Pattern from Rigtorp's
    SPSC writeup + Aeron's `headCachePositionIndex`. On lx64 + 3 KiB
    bodies this is only ~2 % alone (workload is memcpy-bound not
    atomic-bound), but it's the right shape and lets the throughput
    path scale on tinier-message workloads.
  - **v1.5 — `ShmemConnection::call_server_streaming_view<F>`**
    additive zero-copy API. Each Data frame's payload is delivered
    to the caller's `FnMut(&[u8])` closure as a borrow into the
    shared-memory ring slot, with no `memcpy` out of shmem and no
    per-frame heap allocation. Standard
    `call_server_streaming` (owned `Vec<Vec<u8>>`) remains for
    callers who need owned bodies.
  - **v1.5 — `Ring::pop_view_cached` + `Ring::advance_reader`** —
    primitives backing the zero-copy view. Borrow lifetime tied to
    `&self`; caller releases by calling `advance_reader`.
  - **v1.5 throughput numbers**: lx64 (Intel i7-10700K) shmem
    throughput **1.49 → 34.04 GB/s** (23×); macOS arm64 5.20 →
    43.91 GB/s. RFC § 8 > 5 GB/s gate **6.8× over** on lx64.
  - **v1.6 — JS-side metadata block codec** in the TS transport
    packages (`@grpcx/transport-shmem`, `@grpcx/transport-uds`,
    `@grpcx/transport-tcp-mux`). Replaces N-API
    `encodeMetadataBlock` / `decodeMetadataBlock` cross-boundary
    calls with inline TS implementations (`jsEncodeMetadataBlock`,
    `jsDecodeMetadataBlock`) reading/writing the wire bytes
    directly. Drops 1 N-API crossing + 1 V8 ArrayBuffer alloc per
    outbound frame and 1 N-API crossing + N+1 V8 Uint8Array allocs
    per inbound metadata-bearing frame. Adds a unary fast path that
    returns `recvBuf.subarray(0, plen)` zero-copy view when
    `expectSingle && END_STREAM`. **lx64 TS shmem-rtt 18.25 → 12.28
    µs (-33 %), TS uds-rtt 24.50 → 20.41 µs (-17 %)**. The N-API
    `encodeMetadataBlock` / `decodeMetadataBlock` exports remain
    available in `@grpcx/native` for callers that prefer the C++
    codec.
  - **v1.7 — `ShmemConnection.setBusyPollUnbounded(boolean)`** on
    the TS side and `SHMEM_BUSY_POLL=1` env-driven busy-poll mode
    on the Rust `shmem_test_server` example. When enabled symmetrically,
    both ends drop the per-RPC `wakeWrite` + `wakeRead` eventfd
    syscalls and spin on the ring in user space. Mirrors the
    Rust-side `set_busy_poll_unbounded` from v1.3 on the TS surface.
  - **v1.7 — `beginMetaCache` per-(service, method, kind) cache** of
    the pre-encoded 3-entry pseudo-header bytes inside
    `@grpcx/transport-shmem`. When `requestMeta` is empty (the bench
    hot path and the typical non-streaming call shape), the cached
    block is reused — no `Buffer.from(service)` /
    `Buffer.from(method)` / `jsEncodeMetadataBlock` per call.
  - **v1.7 TS shmem RTT numbers**: lx64 12.28 → **5.44 µs (-56 %)**;
    cumulative from v1.5 baseline 18.25 → 5.44 µs = **-70 % / 3.3×**.
    macOS arm64 9.29 → **2.25 µs** (1.96 µs min). 5-6 µs Node + N-API
    idiomatic floor (Phase A predicted, v1.7 measured) is the
    earned ceiling on Node; sub-µs requires Bun/Deno FFI bypass.
  - **v1.8 — JS-side `jsEncodeHeader` / `jsDecodeHeader`** in
    `@grpcx/transport-uds` and `@grpcx/transport-tcp-mux`. Builds /
    reads the 16-byte wire header (`stream_id u32 LE | frame_type u8 |
    flags u8 | metadata_count u16 LE | payload_len u32 LE | reserved`)
    via `Buffer.allocUnsafe(16)` + 6 `writeUInt*LE` / `readUInt*LE`
    calls — zero N-API crossings on the hot path. The N-API
    `encodeHeader` / `decodeHeader` exports remain available in
    `@grpcx/native`.
  - **v1.8 — `beginMetaCache` per-(service, method, kind) cache**
    in `@grpcx/transport-uds`, mirroring the v1.7
    `@grpcx/transport-shmem` change. No-user-metadata calls reuse
    the cached 3-entry pre-encoded pseudo-header block.
  - **v1.8 TS UDS RTT numbers**: lx64 20.09 → **15.98 µs (-20 %)**;
    macOS arm64 15.5 → 10.4-12.1 µs (-25 %). Phase A had predicted
    ~13 µs idiomatic Node + N-API + AF_UNIX floor (all 5 techniques
    stacked); v1.8 lands A1+A2 (~2 µs saving as predicted, no翻盘).
    A4 (`net.Socket._handle` bypass, 1.5-2 µs further) is deferred
    as risk-controlled v1.9 prepared work.
  - **v1.9 — Linux `eventfd(2)` wake-fd migration** in
    `transport_shmem::wake`. On Linux uses one `eventfd(2)` per
    direction (single fd, 8-byte counter, lighter kernel path); on
    Darwin / FreeBSD / NetBSD keeps `pipe(2)`. New `wake_signal` /
    `wake_wait` helpers hide the per-platform byte width (8 B on
    Linux, 1 B on others). Saves ~10 % of `shmem-rtt-park` median
    on lx64 (Intel i7-10700K, kernel 6.12): 3.97 → **3.54 µs (-11 %)**.
    Smaller than the ~30-40 % naive estimate because Linux 6.12
    `pipe(2)` is already well-optimized. The RFC § 8 < 3 µs gate
    is now 1.18× over (was 1.32×); closing it fully would need
    "consumer-blocked" flag in shmem to enable conditional
    `wake_signal` — deferred (the implementation would need an
    additional atomic field in the rendezvous'd region plus
    Acquire/Release coordination around `wake_wait`).
- **`transport-tcp-mux` v2** (raw-TCP wire-v2 mode, parallel to
  the existing v1 mux-frame `transport-tcp-mux`).
  - `TcpMuxV2Connection::connect(addr, authority)` + the four
    call shapes; TCP_NODELAY on by default.
  - `Server::listen_tcp_mux_v2[_until]`.
  - `Server::listen_tcp_mux_v2_busy_poll_until(addr, stop)` —
    deployer-opt-in user-space busy-poll serve loop: bypasses
    the SQPOLL + linux_iouring fast paths, sets the accepted
    socket non-blocking, and spins `read_frame` on `WouldBlock`
    instead of paying a `read(2)` syscall per RPC. Closes the
    RFC § 8 < 8 µs gate on lx64 (8.2 → 6.6 µs median tcpmux-rtt;
    new bench flag `--tcpmux-server-busy-poll`). Costs one CPU
    per active connection on the server.
  - `TcpMuxV2ServerConn` reserved.
- **`transport-tcp-mux-iouring`** feature (opt-in, Linux only at
  the cfg level — no-op on non-Linux) — io_uring per-connection
  fast path for the `transport-tcp-mux v2` server. v1 wiring uses
  submit-and-wait per syscall; benched on Linux 6.12 it currently
  *loses* to the T4.1 blocking serve (folds into the v1.1 perf
  workstream that upgrades to SQPOLL + multishot recv + linked-
  send pair).
- **`wire_v2` module** — frame codec for the v2 transports.
  `Header` (16 B fixed) / `FrameType` (Begin / Data / End / Reset
  / Ping / Pong) / `Flags` (END_STREAM / HAS_METADATA /
  CONTINUATION) / `MetadataEntry` + `encode_metadata_block` /
  `decode_metadata_block` + the constant-time `decode_header` /
  `encode_header`. KAT vectors at `_dev/kat-vectors/9.{1,2,3,4}*.hex`
  validate against both the Rust codec (`kat_wire_v2`) and the TS /
  C++ codec (`@grpcx/native`).
- **`wire_v2::pseudo` submodule** — hot-path fast helpers for
  the v2 BEGIN/END pseudo-headers: `STATUS_OK_BLOCK`
  (17-byte pre-baked `:status = OK` END metadata literal),
  `encode_begin_block(out, service, method, kind, user)` stack-
  buffer writer, plus the `PSEUDO_*` name constants shared
  across the three transports. UDS client `send_begin` and
  server `send_response` now take a 128-byte-stack-buffer fast
  path on the no-user-metadata case (the bench hot path), and
  splice `STATUS_OK_BLOCK` directly into END frames without
  re-encoding it per RPC.
- **`RpcChannel` trait** at the crate root — bridges the v1
  `ConnectedChannel` and every v2 transport's connection type
  (`UdsConnection`, `ShmemConnection`, `TcpMuxV2Connection`) so
  generated client stubs work over either.
- **`CallContext::peer_cred()`** + `Service::dispatch_with_ctx`
  — v2 ctx-aware dispatch entry point with kernel-attested peer
  credentials threaded through.

### Fixed

- **Linux `struct cmsghdr` SCM_RIGHTS width** — `cmsg_len` is
  `size_t` (8 B) on Linux glibc, `socklen_t` (4 B) on
  Darwin/FreeBSD. The shmem rendezvous code hard-coded `u32`;
  every shmem call hung at "server never bound" on Linux. Fixed
  in `transport_shmem::rendezvous` with `cfg(target_os = ...)`-
  conditional `CmsgHdr` + `CMSG_BUF_LEN_3FD` + `cmsg_len_for_3fds`.

### Changed

- **`transport-tcp-mux v2` Linux serve — v1.1 SQPOLL
  event loop** (gated `feature = "transport-tcp-mux-iouring"`).
  New `transport_tcp_mux_v2::linux_iouring_v2` module
  rewrites the per-connection serve as SQPOLL ring +
  cqe-driven event loop + single-SEND-per-frame
  (replaces v1's `submit + submit_and_wait` per syscall
  shape). Fallback chain on setup failure: v1.1 SQPOLL →
  v1 plain ring → T4.1 blocking. Measured tcpmux-rtt on
  lx64 (kernel 6.12, Debian 13) at **11.59 µs median**
  (10 000 samples × 3 runs warm), 1.15× faster than
  T4.1 blocking and 1.64× faster than v1 iouring.
- **`transport-tcp-mux v2` Linux client — v1.1.1
  symmetric io_uring** (same feature gate). New
  `transport_tcp_mux_v2::linux_iouring_client` module
  routes `TcpMuxV2Connection::call_unary` through a
  plain (non-SQPOLL) ring: SEND + RECV submitted as a
  batch, drained by one `io_uring_enter(GETEVENTS)`
  per CQE wait instead of three separate blocking
  syscalls (writev + 2 × read_exact). Plain ring (not
  SQPOLL) is intentional — the bench harness runs
  client + server in the same process, and two SQPOLL
  threads pinned to the same CPU serialise on the
  scheduler quantum (~4 ms per RPC observed
  experimentally; reverted in favour of the
  syscall-batched plain ring). The bench harness now
  links the `transport-tcp-mux-iouring` feature so
  Linux runs through the symmetric path automatically.
  Measured tcpmux-rtt on lx64 at **9.04 µs median**
  across 3 × 10 000-sample runs (8.99 / 9.04 / 9.20 µs;
  min 8.66 µs), 1.49× faster than T4.1 blocking and
  1.28× faster than v1.1 server-only.
- **`transport-tcp-mux v2` Linux — v1.2 send buffer
  pool** (same feature gate). Both ends drop the
  v1.1 / v1.1.1 `Box::leak`-per-frame pattern in
  favour of recycled buffers:
  - Client (`linux_iouring_client::ClientRing`) holds
    one `send_buf: Vec<u8>` reused across calls; safe
    because `call_unary` strictly alternates send →
    recv (one SEND in flight at a time, drained inside
    `recv_frame::drain_cqes` before the next call).
  - Server (`linux_iouring_v2::Conn`) holds a 64-slot
    `send_slots: Vec<Vec<u8>>` pool keyed by `user_data
    = UD_SEND_BASE + slot_idx`; `drive_loop` returns
    each slot to `free_slots` when its CQE drains.
    Supports the streaming RPC case where multiple
    DATA / END frames are in flight on the same
    connection.
  - **Perf verdict**: measured tcpmux-rtt on lx64 at
    9.04 / 9.09 / 9.22 µs median across 3 × 10 000
    runs (min 8.62 µs) — **within noise of v1.1.1**.
    The per-frame allocator cost was not the
    bottleneck on glibc's tcmalloc-fast-path. v1.2 is
    a correctness / no-allocation-growth win for
    long-running services, not a perf win.
- **`TcpMuxV2Connection::set_busy_poll_threshold(Duration)`**
  — v1.2.1 client-side busy-poll opt-in (same API
  shape as `ShmemConnection::set_busy_poll_threshold`).
  Default `Duration::ZERO` (blocking-wait path,
  v1.2 behaviour unchanged). When set, the client's
  io_uring recv path submits SQEs without a wait and
  spins on the CQ ring (memory-mapped, syscall-free)
  for up to the window, falling back to
  `io_uring_enter(WAIT)` on exhaustion.
- **`internal-rpc-bench --tcpmux-busy-poll-us N`** flag
  added to the bench harness so the opt-in can be
  measured alongside the default.
- **Honest RFC § 8 verdict for `tcpmux-rtt`**: lx64
  measurements with `--tcpmux-busy-poll-us` swept at
  10 / 50 / 100 µs come in at 9.21 / 9.18 / **8.94 µs**
  median (min 8.43 µs at the 100 µs window) vs 9.22 µs
  baseline — **a marginal ~3 % improvement**. `perf`
  attribution (`grpcx-v1-bench/_dev/v1.2-perf.data`):
  21 % of cycles are in the server-side SQPOLL kernel
  thread (`io_sq_thread` + `tctx_task_work_run`); 5.8 %
  in `clear_bhb_loop` (Spectre branch-history
  mitigation on syscall entry); only ~8 % in client-side
  syscall path. Client busy-poll skips the 8 % but
  leaves the dominant 21 % SQPOLL overhead untouched.
  **The < 8 µs gate is not reachable from the client
  side alone** — closing it requires switching the
  server from SQPOLL to user-space busy-poll on the
  serve thread, which is an idle-connection-CPU
  trade-off (server CPU now scales with connection
  count, not RPC rate). That architectural decision is
  deferred to the deployer; recommended-reading is to
  **document the gate at 9 µs on Linux loopback for
  the SQPOLL-server + sync-client topology**, and
  expose the opt-in busy-poll knob for callers who
  control the CPU budget.

### Notes (RFC § 8 perf gates)

T5.4 bench harness (`comparators/internal-rpc/` + matching TS
driver at `ts/packages/grpcx-bench/`) measured all nine RFC § 8
gates on the Linux reference box. 5 of 9 are 1.1×–1.7× over the
threshold; shmem throughput is 54× under; the opt-in io_uring path
is 1.4× *slower* than the T4.1 blocking serve. Two single-lever
tuning attempts (buffered streaming FrameReader, skip-zero-init on
the metadata buffer) were measured and reverted — neither moved
the numbers. The gap is **structural**, not a tuning miss. v1
ships at measured numbers; the architectural rework (full io_uring
SQPOLL event-loop server, pipelined shmem push) is the **v1.1 perf
workstream**.

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

## grpcx-pb — Unreleased (internal-RPC v1)

### Added
- `codegen_ts(&Schema) -> String` — TypeScript backend emitter for
  the protobuf schema (T1.2 of the internal-RPC v1 plan). Emits
  self-contained `.ts` source with `@grpcx/core` / `@grpcx/native`
  imports, message interfaces, descriptor tables, enum
  declarations, a `class XxxClient` covering all four RPC kinds
  (Promise / AsyncIterable), an `XxxHandler` interface, and the
  server-side `registerXxx` function.
- Rust codegen (`codegen`) now emits all three of `dispatch` /
  `dispatch_streaming` / `dispatch_with_ctx`; generated
  `XxxServer<T>` overrides `Service::dispatch_with_ctx` so
  `CallContext::peer_cred()` threads through to handlers on the
  v2 internal-RPC transports.
- Generated client stubs are generic over the `grpcx::RpcChannel`
  trait, working transparently over `ConnectedChannel` (v1) or
  `UdsConnection` / `ShmemConnection` / `TcpMuxV2Connection` (v2).

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

## grpcx-rt — Unreleased (internal-RPC v1 / T4.2 io_uring)

### Added
- `iouring` feature (Linux-only at the `cfg(target_os = "linux")`
  level — no-op on non-Linux) — runtime-level wrappers around the
  `grpcx-io` Linux io_uring primitives extended in T4.2.
  - `UringRing::new(entries)` / `UringRing::new_sqpoll(entries,
    cpu, idle_ms)` — owning ring handle with SQPOLL-aware submit
    dispatch.
  - `register_files` / `unregister_files` /
    `register_buffers` / `unregister_buffers` —
    `IORING_REGISTER_FILES` / `IORING_REGISTER_BUFFERS` plus
    value-typed `FixedFileTable` / `RegisteredBuffers` records
    for slot lookup.
  - `submit_send` / `submit_send_fixed` / `submit_recv` /
    `submit_recv_multishot` / `submit_linked_send_pair` — typed
    SQE constructors covering the v2-frame hot path.
  - `Completion` + `drain_cqes` — CQE drain helper.
- `iouring_basic` test exercises every primitive (NOP, SQPOLL
  setup with skip-on-EPERM, linked send over socketpair,
  multishot recv with skip-on-`EINVAL`, fixed-file send via
  registered slot) — 5/5 green on Linux 6.12.

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

## grpcx-io — Unreleased (internal-RPC v1 / T4.2 io_uring)

### Added (Linux io_uring primitive extensions)
- `Ring::new_sqpoll(entries, cpu, idle_ms)` — SQPOLL +
  `IORING_SETUP_SQ_AFF` ring construction; surfaces kernel
  refusals (kernel < 5.11 without `CAP_SYS_NICE`, locked-down
  kernels) as `IoErrorKind::BackendUnavailable` so callers have
  a single fallback branch to `Ring::new`.
- `Ring::submit_sqpoll(to_submit)` — fast-path submit that
  checks `IORING_SQ_NEED_WAKEUP` and skips the syscall when the
  kernel polling thread is awake.
- `Ring::register_files` / `unregister_files` +
  `Ring::register_buffers` / `unregister_buffers` —
  `IORING_REGISTER_FILES` / `IORING_REGISTER_BUFFERS` plus their
  unregister counterparts. The `io_uring_register` syscall
  wrapper + `IoVec` struct are exposed at the raw layer.
- `Sqe::send` / `Sqe::recv` / `Sqe::recv_multishot` — typed SQE
  constructors for `IORING_OP_SEND` / `IORING_OP_RECV` plus the
  multishot recv variant (kernel >= 6.0).
- `Sqe::linked()` / `Sqe::with_fixed_file()` — builder
  modifiers for `IOSQE_IO_LINK` (chain to the next SQE) and
  `IOSQE_FIXED_FILE` (interpret the `fd` field as a registered
  file-table index).

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
