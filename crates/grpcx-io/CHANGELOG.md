# Changelog

All notable changes to `grpcx-io` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.4.0] below)

## [1.4.0] - 2026-06-12

### Added
- `IoErrorKind::ConnectionRefused` (`#[non_exhaustive]`
  enum — additive): linux errno 111 / darwin + freebsd 61.
  On connected UDP sockets this is the kernel surfacing an
  ICMP port-unreachable; L10 builds its
  handshaking-vs-established refusal policy on it.

## [1.3.0] - 2026-06-12

**Additive minor** — the Linux side of the batch-datagram
path (named-and-deferred at 1.2.0: "untested syscall
numbers don't ship"; a Debian 13 / kernel 6.12 x86_64 box
now verifies them).

### Added
- `udp_batch` module on Linux: `send_batch` / `recv_batch`
  over `sendmmsg` / `recvmmsg`, same contract as the
  Darwin pair (CONNECTED non-blocking UDP sockets only,
  ≤64 datagrams per syscall, short count on send-buffer
  pressure, `WouldBlock` when zero move; flags 0,
  `recvmmsg` timeout NULL).
- x86_64 numbers (299/307) hw-verified by loopback
  round-trip KATs (`tests/udp_batch_linux.rs`, the Darwin
  precedent: the round trip IS the layout verification).
  aarch64 numbers (243/269) come from the same asm-generic
  table as the shipped epoll/eventfd entries —
  runtime-unverified (no arm64 Linux box), recorded in
  `REFACTOR-v10-linux-sendmmsg.md`.

## [1.2.0] - 2026-06-11

**Additive minor** — layer-up patch for L10's UDP batch
path: bulk gRPC responses leave as dozens of MTU-sized
datagrams per pump, and per-datagram syscalls dominated
the large-payload e2e lanes.

### Added
- `udp_batch` module (Darwin/iOS only): `send_batch` /
  `recv_batch` over the XNU batch-datagram syscalls
  `sendmsg_x` / `recvmsg_x` (numbers verified against the
  macOS SDK `sys/syscall.h`; `struct msghdr_x` layout
  verified by loopback round-trip tests against real
  connected socket pairs). Up to 64 datagrams per
  syscall; CONNECTED non-blocking sockets only (the
  kernel rejects `msg_name` on this path).
- Other platforms: no new surface — callers fall back to
  per-datagram loops (Linux `sendmmsg`/`recvmmsg` is a
  future addition once it can be verified on a Linux
  box; the syscall numbers will not be shipped untested).

### Notes
- The workspace package version tracks this crate's
  released surface — workspace 1.1.0 → 1.2.0.

## [1.1.0] - 2026-05-29

**Additive minor** — fills the one `mio` API the v1.0 surface was
missing: a cross-thread reactor wakeup. Backward compatible (nothing
removed or changed), so v1.0 callers keep working. Driven by the L2
runtime's need to unpark its IO driver when a task is woken from another
thread (`grpcx-rt` v0.4 channels / v0.5 multi-thread). See
[../../REFACTOR-v1.1-waker.md](../../REFACTOR-v1.1-waker.md).

### Added
- **`Waker`** — `mio::Waker` parity. `Waker::new(&Registry, Token)`
  binds it to a reactor; `wake()` (callable from any thread) makes the
  bound `Poll::poll` return an event for that token; `reset()` re-arms
  after the token is observed.
  - Linux: `eventfd` (new `eventfd2` inline syscall; `EFD_NONBLOCK` so
    draining never blocks). `reset()` drains the counter (epoll is
    level-triggered).
  - macOS / iOS / FreeBSD: `kqueue` `EVFILT_USER` + `NOTE_TRIGGER`
    (`EV_CLEAR` auto-resets, so `reset()` is a no-op). FreeBSD's
    `EVFILT_USER` is `-11` vs Darwin's `-10`.
- `Registry` gains `pub(crate)` `register_user` / `trigger_user` /
  `deregister_user` (kqueue) and the Linux waker reuses `ctl_add`.
- 3 cross-platform conformance tests in `tests/waker.rs`: same-thread
  wake, **cross-thread wake of a blocked `poll`**, reset-then-wake loop.

### Validated
- Linux x86_64 (lx64, eventfd): 3 waker + 40 conformance ✅
- macOS aarch64 (kqueue): 3 waker + 34 conformance ✅
- FreeBSD 15.0 amd64 (kqueue, lx64 KVM): 3 waker + 34 conformance ✅

### Note
- 0-dep intact: `eventfd` / `EVFILT_USER` are syscalls, no new crate.
  `DEPS_AUDIT.md` unchanged.

## [1.0.0] - 2026-05-29

**Layer 1 closed.** All lab-internal v1 trigger items green; semver-
stable release. Code, tests, benches, docs unchanged from `[0.10.0]`
— this is the "tag the green state" release.

### What v1.0 means
- **API surface stable.** Future v1.x patch / minor releases preserve
  the public surface (`Poll`, `Registry`, `Events`, `Event`, `Token`,
  `Interest`, `Ready`, `IoError`, `IoErrorKind`, `IoResult`,
  `SourceFd`, plus the Linux-only `Ring` / `Sqe` / `Cqe`). Breaking
  changes would require a v2.0 (which by the project's
  version-number-mapping rule starts the `grpcx-rt` series).
- **Cross-platform**: Linux (epoll + io_uring), Darwin macOS / iOS
  (kqueue), FreeBSD (kqueue). Windows IOCP is post-v1 (see
  `REFACTOR-v1-v0.6-windows-iocp.md`).
- **Zero deps** for runtime code (`[dependencies]` empty). Dev-dep
  exceptions tracked in `DEPS_AUDIT.md` (`criterion` + `mio` for
  benches; `libfuzzer-sys` for the workspace-excluded `fuzz/`
  subcrate).
- **Performance**: parity-or-better with `mio` on every measured
  bench across 3 OSes; io_uring lane **1.29× faster than mio epoll**
  at N=256 fan-in.
- **Footprint**: 644 B (0.3 %) contribution to a 308 KB Linux x86_64
  release binary; zero heap allocations per iteration on every hot
  path.
- **Stability**: 49 billion fuzz runs (24 h CPU-hours cumulative)
  across two fuzz targets, **zero crash / zero leak / zero
  timeout**.

### Changed
- Workspace package version `0.1.0` → `1.0.0`.
- `crates/grpcx-io/Cargo.toml` switches to `version.workspace = true`
  inherit (was a literal `0.1.0`).

## [0.10.0] - 2026-05-29

Closes the last three lab-internal v1-trigger items: coverage ≥ 95%
(measured 95.48% on canonical Linux), 24h cumulative fuzz with **zero
crash** across two targets (49 billion total runs), MEM/BINSIZE
numbers (308 KB total stripped binary; `grpcx-io` contributes 644 B
/ 0.3%; zero heap allocs per iter on every hot path). **L1 v1.0 is
unblocked.**

### Added
- `cargo-fuzz` harness at `crates/grpcx-io/fuzz/` with two fuzz
  targets:
  - `decode_one_event` — per-OS event decoder fuzz via the new
    `__fuzz_helpers::decode_one_event` doc-hidden API.
  - `lifecycle` — `Poll` / `Registry` / `SourceFd` state-machine
    fuzz driven by a fuzz-byte script (32 ops per invocation cap).
- `__fuzz_helpers` doc-hidden module exposing `decode_one_event`
  and `interest_from_bits` so the fuzz subcrate (outside the
  workspace) can reach the `pub(crate)` translation funcs.
- 33 new unit tests:
  - 16 tests in `src/types.rs` covering every `pub fn` /
    `Debug` impl on `Token` / `Interest` / `Ready` / `IoError` /
    `IoErrorKind` / `IoResult`.
  - 4 tests in `src/lib.rs` exercising `__fuzz_helpers`.
  - 1 test for `__bench_helpers` round-trip in `sys/linux/tests.rs`.
  - 5 new Linux conformance tests covering `WRITABLE` / `PRIORITY`
    interest paths, `IntoIterator for &Events`, double-register
    `AlreadyRegistered`, invalid-fd `read` for `InvalidInput`, and
    `Duration::saturating` clamp.
  - 2 io_uring tests (`Sqe::write` round-trip, `Cqe Debug + Copy +
    Clone`, `Ring::submit` without wait).
  - 4 tests in `sys/darwin/tests.rs`: `__bench_helpers` round-trip,
    register-all-three-filter-arms, reregister-narrow-drops-filters,
    invalid-fd-read. Lifts Darwin coverage 89.57% → 92.65%.
  - 4 tests in `sys/freebsd/tests.rs` mirroring the Darwin set
    (PRIORITY interest silently dropped per FreeBSD's lack of
    `EVFILT_EXCEPT`).
- `event_translate_amortized_under_budget` perf_gate test: asserts
  the per-event decoder costs < 50 ns amortized (observed ~250-300
  ps on apple silicon mac + linux x86_64; uses a 1000-call batch
  to skirt `Instant::now()`'s ~40 ns overhead).
- `examples/min_poll.rs` — smallest binary using the L1 API. Used
  as the `cargo bloat` measurement target for BINSIZE tracking.
- Static struct-size assertions (`const _: () = assert!(size_of::
  <T>() == N)`) in `src/types.rs` + `src/event.rs` for `Token`,
  `Interest`, `Ready`, `IoError`, `IoErrorKind`, `Event`. Field-
  layout drift now fails `cargo check`.
- `crates/grpcx-io/BUDGETS.md` MEM tier static-size and heap-
  footprint tables populated with measured / asserted numbers.
- `crates/grpcx-io/BUDGETS.md` BINSIZE tier filled — 308 KB total
  Linux x86_64 stripped binary; **644 B (0.3%) attributable to
  `grpcx-io`**.
- `compare_mio/event_translate` bench — isolates the per-event
  translate cost from any syscall (uses `__fuzz_helpers`).
- `compare_uring/ring_new` bench — `Ring::new(8)` + `Ring::new(64)`
  allocation cost (Linux only).

### Fixed
- `lifecycle` fuzz target initially called `read(rfd, ...)` on
  blocking pipes, which deadlocked the fuzz invocation when the
  pipe was empty (libFuzzer's 20-min per-test timeout then
  amplified the issue). Removed the read case; the state-machine
  surface is exercised fully by register/reregister/deregister/
  poll/write.

## [0.9.0] - 2026-05-28

Charter decisions locked + design docs synchronized + GH Actions CI
scaffolded. No runtime code change beyond one private-intra-doc-link
fix in `ring.rs`. The five charter decisions:

1. **L3 crypto** lands as a `rustls` + `*ring*` wrapper initially —
   self-implementation deferred until an external cryptographer
   review window opens. Listed as an explicit, time-bounded 0-dep
   exception in `DEPS_AUDIT.md`.
2. **L7 ↔ L8 swap** — our own IDL (zero-copy wire) becomes L7,
   `.proto` importer becomes L8. Implementation order now matches
   the layer number.
3. **Fuzz cadence**: 24h cumulative mandatory on L1 / L2 /
   L3-wrapper / L4; ≥ 6h opportunistic on L5–L9; L10 24h
   integration stress.
4. **L10 negotiation strategy**: ALPN (`grpcx/h3` preferred,
   `grpcx/ws-mux` fallback) for client-server; application-level
   capability detection + `Alt-Svc` for browser.
5. **Project priorities**: perf > stability > mem > binsize. Every
   design tradeoff resolves in that order.

### Added
- `ARCHITECTURE.md` "Priorities (project-wide)" section codifying
  the perf > stability > mem > binsize order plus concrete
  derivation rules.
- `crates/grpcx-io/BUDGETS.md` **MEM tier** section (methodology +
  static-sizes table + per-instance heap-footprint table; numbers
  TBD in v0.10 via `dhat`).
- `crates/grpcx-io/BUDGETS.md` **BINSIZE tier** section (methodology
  via `cargo bloat --release` against `examples/min_poll.rs`;
  numbers TBD in v0.10).
- `PERFORMANCE.md` **Reproducibility** section with exact 3-platform
  bench commands + hardware/OS snapshot table.
- `.github/workflows/test.yml` matrix-style gating: native (Linux +
  macOS) + cross (aarch64-linux + freebsd) + iOS cross-compile.
- `.github/workflows/bench.yml` scaffold (workflow_dispatch printing
  manual SSH commands; real automation pending self-hosted runner
  setup).

### Changed
- `ROADMAP.md`: L3 entry split into interim (rustls wrapper) +
  final (self-impl) forms with documented retire-when condition;
  L7 ↔ L8 swap; L10 negotiation strategy section added; "Lab-
  internal scope" section codifies which trigger items are gating
  (lab-internal) vs deferred (external).
- `ARCHITECTURE.md`: "Layer-promotion trigger" split into lab-
  internal (gating) + external (deferred) tables.
- `DEPS_AUDIT.md`: new "L3 crypto interim wrapper" exception
  section + "Contrast with mailrs" Crypto row updated.
- `docs/v1-scope.md`: v1 trigger reduced to lab-internal items;
  v0.x partition table updated (v0.8 ✅, v0.9 ✅, v0.10 next).
- `README.md` (workspace) + `crates/grpcx-io/README.md` rewritten
  to reflect v0.8 state and the five charter decisions.

### Fixed
- `crates/grpcx-io/src/sys/linux/io_uring/ring.rs:64` private
  intra-doc link (`[module docs](self)`) rewritten as inline prose;
  was breaking `cargo doc -D warnings` on Linux cfg targets only.

## [0.8.0] - 2026-05-28

Bench suite publish + first cross-OS `PERFORMANCE.md` consolidation.
Lands two `grpcx-io` optimizations that the new benches surfaced.
No backend additions; cross-platform conformance + perf-gate
remain green on Linux / Darwin / FreeBSD.

### Added
- `compare_mio.rs` grows from 2 to 6 bench functions:
  `reregister_only`, `poll_drain[N=8/64/256]`, `poll_new`,
  `events_with_capacity_64`. The poll-drain bench measures the
  full fan-in round-trip (N writes + 1 poll + N translates +
  N reads) — the workload the L1 reactor is intended for.
- `PERFORMANCE.md` gets a cross-OS summary table at the top
  consolidating Linux / Darwin / FreeBSD numbers per operation
  against the corresponding `mio` lane.
- `BUDGETS.md` HOT-tier observed columns filled across 3 OSes
  (24 rows); 5 of 7 WARM rows filled (`Poll::new`,
  `Events::with_capacity(64)` × 3 OSes each). `Ring::new(8)` and
  per-event translate microbenches deferred (no isolation harness
  yet).
- `BUDGETS.md` table now splits per-OS for `reregister` and
  `poll_drain N=*` — kqueue + epoll have meaningfully different
  cost classes.

### Changed
- `BUDGETS.md` budget for `Poll::poll drain N=8/64/256` updated
  to match the actual `compare_mio/poll_drain` workload shape
  (was: optimistic numbers from before the bench existed).

### Optimized
- `Events::poll_with` no longer pays a per-call `clear + resize`
  memset on the kernel-side scratch buffer. After the first
  `poll()`, the buffer is reused; the kernel writes into
  `0..n` slots and we only iterate the same prefix. Largest win
  on FreeBSD where `Kevent` is 64 B: **poll_idle 159 ns → 124 ns
  (−22%)**. Linux had a ~7% gain; Darwin's bench is dominated by
  syscall cost so the change is in the noise.
- kqueue `reregister` collapses two `kevent` syscalls (deregister
  then register) to **one** syscall using a complement-DEL
  change list. `EV_ADD` slots for filters in the new interest
  set are idempotent (refresh udata in-place); `EV_DELETE` slots
  for filters not in the new set tolerate `ENOENT`. Change list
  is exactly 3 entries on Darwin (2 on FreeBSD) regardless of
  interest pattern. Darwin reregister: **470 ns → 301 ns
  (−36%)**, now within 10% of mio. FreeBSD reregister: 444 ns
  → 297 ns, beats mio by 4%. Linux unaffected (already one
  `EPOLL_CTL_MOD` syscall).

## [0.7.0] - 2026-05-28

FreeBSD `kqueue` backend joins `epoll` + `io_uring` (Linux) and
`kqueue` (Darwin) as the fourth L1 backend. Validated on a
KVM-accelerated FreeBSD 15.0-RELEASE VM hosted on lx64 (Debian 13 /
i7-10700K). No cross-platform regression: Linux 9/9 conformance +
3/3 perf_gate, Darwin 10/10 conformance, FreeBSD 10/10 conformance
+ 2/2 perf_gate. v0.6 (Windows IOCP) was removed from v1 scope
earlier the same day; v0.7 is the direct successor to v0.5 in the
active checkpoint chain. See `PERFORMANCE.md` for head-to-head
numbers against `mio` on the FreeBSD kqueue lane.

### Added
- FreeBSD inline-asm syscall layer (`sys::freebsd::raw`):
  - Carry-flag error-path helpers `syscall3` / `syscall6` for
    `x86_64-unknown-freebsd`. The amd64 ABI hybrid is
    Linux-register-convention + Darwin-carry-flag-error.
  - **r8 / r9 / r10 declared as clobbered** — FreeBSD's amd64
    syscall handler uses them as scratch (it moves `r10 → rcx`
    for the C-ABI arg4 slot) and does not restore them. Linux
    and Darwin both preserve those registers; the difference
    only manifests under `opt-level=3` and caused 100% SIGSEGV
    in early release builds. Documented in `sys::freebsd::raw`'s
    module-level comment for future maintainers.
  - `close` / `read` / `write` / `fcntl` / `pipe_cloexec` safe
    wrappers. FreeBSD has a native `pipe2(O_CLOEXEC)` syscall
    (`SYS_pipe2 = 542`) — one atomic call sets `O_CLOEXEC` with
    no `fcntl` follow-up, no thread race against `exec*`.
  - `kqueue` / `kevent` syscall wrappers plus the FreeBSD-shape
    `Kevent` struct (64 bytes — leading 32 match Darwin, trailing
    `ext[4]` is FreeBSD-specific) and `Timespec` (16 B) with
    compile-time `size_of` guards.
- FreeBSD `Selector` (`sys::freebsd::kqueue`):
  - `register` / `reregister` / `deregister` mirror Darwin's
    behavior; `EV_RECEIPT` on every change-list entry; `EV_CLEAR`
    edge-triggered semantics matching mio.
  - `Interest::PRIORITY` is **silently dropped** — FreeBSD has no
    `EVFILT_EXCEPT`. Documented in the module-level comment.
  - `deregister` returns `IoErrorKind::NotFound` when both filter
    `DELETE` calls report `ENOENT`, mirroring the v0.2 epoll and
    v0.5 Darwin contracts.
- `Poll` / `Registry` / `Events` / `SourceFd` widened to drive the
  FreeBSD `Selector` on `target_os = "freebsd"`.
- `RawFd` and `SourceFd` are now publicly re-exported on FreeBSD
  builds; `Events::poll_with` carries one cfg-gated branch
  covering Darwin + FreeBSD (since the renamed `Kevent` type is
  abstract over the OS).
- `__bench_helpers` re-exposed on FreeBSD; `pipe2(O_CLOEXEC)` is a
  direct shim around `raw::pipe_cloexec` (which is itself a single
  `SYS_pipe2` syscall).
- 10 FreeBSD conformance tests (`sys::freebsd::tests`) byte-for-
  byte parallel to Darwin's, covering syscall ABI smoke, Selector
  direct, lifecycle (incl. second-deregister `NotFound`), public
  `Poll` round-trip, and timeout behavior.
- `benches/compare_mio.rs` and `tests/perf_gate.rs` widened to
  FreeBSD; per-OS perf budgets gain a FreeBSD branch. Observed
  on the lx64 KVM VM: 159 ns poll-idle (mio: 131 ns), 444 ns
  register+dereg (mio: 451 ns).

### Changed
- Internal type rename: `DarwinKevent` → `Kevent` inside
  `sys::darwin::raw` and re-exports. The Darwin-suffixed name
  baked the OS into a struct whose abstraction is "the kqueue
  family"; the cleaner `Kevent` lets `sys::darwin` and
  `sys::freebsd` both export a same-named concrete type that
  `event.rs` consumes through one cfg branch. Internal change
  only — no public API surface affected.

### Fixed
- Release-mode SIGSEGV in `sys::freebsd::raw::syscall3` /
  `syscall6` (see `Added` note above re: r8/r9/r10 clobbers).
  Debug builds were unaffected because they don't schedule live
  values across the inline-asm block; release builds did and
  segfaulted on the next register dereference.

## [0.5.0] - 2026-05-28

Darwin (macOS / iOS) `kqueue` backend joins `epoll` + `io_uring` as
the third L1 backend. Validated on Apple Silicon (Darwin 25.5.0); no
regression on lx64 Linux. See `PERFORMANCE.md` for head-to-head
numbers against `mio` on the kqueue lane.

### Added
- Darwin inline-asm syscall layer (`sys::darwin::raw`):
  - Carry-flag error-path helpers `syscall3` / `syscall6` for both
    `x86_64-apple-darwin` (`syscall` instruction with the BSD-class
    `0x2000000` prefix) and `aarch64-apple-darwin` (`svc #0x80`
    with `x16` carrying the syscall number).
  - `close` / `read` / `write` / `fcntl` / `pipe_cloexec` safe
    wrappers, the last of which handles Darwin's register-pair
    return shape (`(rax, rdx)` on x86_64 / `(x0, x1)` on aarch64)
    and applies `FD_CLOEXEC` via `fcntl`.
  - `kqueue` / `kevent` syscall wrappers plus the
    `DarwinKevent` (32 B) + `Timespec` (16 B) kernel ABI structs
    with compile-time `size_of` guards.
- Darwin `Selector` (`sys::darwin::kqueue`):
  - `register` / `reregister` / `deregister` map the
    cross-platform `Interest` bitset to `EVFILT_READ` / `_WRITE` /
    `_EXCEPT` filters with `EV_CLEAR` edge-triggered semantics —
    matches `mio` for cross-OS parity.
  - `EV_RECEIPT` is part of every change so `kevent` returns
    synchronously after applying the change list; `timeout = NULL`
    is safe in that mode (~30× faster than `timeout = {0,0}`).
  - `deregister` returns `IoErrorKind::NotFound` when every filter
    `DELETE` reports `ENOENT`, mirroring the v0.2 epoll contract.
- `Poll` / `Registry` / `Events` / `SourceFd` widened to drive the
  Darwin `Selector` on `target_os = "macos" | "ios"`.
- `RawFd` and `SourceFd` are now publicly re-exported on Darwin
  builds; `Events::poll_with` translates kqueue events into the
  same `Event { token, ready }` shape Linux's epoll path emits.
- `__bench_helpers` re-exposed on Darwin with the same surface as
  Linux's; `pipe2(O_CLOEXEC)` is a thin shim around
  `raw::pipe_cloexec`.
- 10 Darwin conformance tests (`sys::darwin::tests`) covering
  syscall ABI smoke, Selector direct, lifecycle (incl. second-
  deregister `NotFound`), public `Poll` round-trip, and timeout
  behavior.
- `benches/compare_mio.rs` and `tests/perf_gate.rs` widened to
  Darwin; perf budgets are now per-OS (Darwin's `kevent` is ~150×
  slower than Linux's `epoll_pwait` for an empty wait — the budget
  reflects that).

### Fixed
- A bug surfaced by the bench: the first cut of Darwin `register`
  omitted `EV_RECEIPT` from its change-list flags. When combined
  with the later `timeout = NULL` optimization the kernel blocked
  forever waiting on events. Adding `EV_RECEIPT` everywhere fixed
  the hang and collapsed `register + deregister` from 12.9 µs to
  458 ns.

## [0.4.0] - 2026-05-27

Validated on lx64 (Intel i7-10700K @ 3.80 GHz, Debian 13 kernel
6.12.74). `Ring` median 91.88 µs vs `Poll` (epoll via mio) 116.21 µs
at N = 256 — uring 1.26× faster; v0.4 trigger gate green. See
`PERFORMANCE.md` for the full table.

### Added
- Linux `io_uring` backend exposed as a **sibling** to `Poll`, not a
  substitute:
  - `Ring::new(entries)` — allocate a ring; falls back via
    `IoErrorKind::BackendUnavailable` on kernels < 5.1 / sandboxed
    environments (`ENOSYS` / `EINVAL` / `EPERM` from
    `io_uring_setup`).
  - `Ring::push_sqe(&Sqe)` — single-producer publish.
  - `Ring::submit` / `Ring::submit_and_wait` — kernel notify.
  - `Ring::reap_cqes(F: FnMut(Cqe))` — single-consumer drain.
- `Sqe::nop` / `Sqe::read` / `Sqe::write` — opcode constructors.
  `read` / `write` are `unsafe fn` because the kernel reads / writes
  the caller's buffer asynchronously; the buffer's lifetime cannot be
  expressed at the type level.
- `Cqe { user_data, res, flags }` — typed completion value.
- `RawFd = i32` (Linux) promoted to the public API surface for use
  in `Sqe` constructors.
- Inline syscall layer extended via `core::arch::asm!`:
  `io_uring_setup`, `io_uring_enter`, `mmap`, `munmap`. Zero new
  external dependencies.
- Compile-time `size_of::<T>()` assertions on every kernel-ABI
  struct (`IoUringParams`, `IoSqringOffsets`, `IoCqringOffsets`,
  `IoUringSqe`, `IoUringCqe`) to fail-fast on layout drift.
- Single-mmap (`IORING_FEAT_SINGLE_MMAP`, kernels ≥ 5.4) is detected
  at runtime; `Ring` collapses to two mmaps when supported, three
  otherwise.

## [0.2.0] - 2026-05-27

### Added
- Linux `epoll` backend on x86_64 and aarch64: `Poll::new`,
  `Poll::poll`, `Registry::{register,reregister,deregister}` are all
  live on Linux.
- `SourceFd` — wrap any raw Linux fd as a registerable `Source`.
- Inline syscall layer via `core::arch::asm!` — no `libc`, no
  `windows-sys` added. Covers `epoll_create1`, `epoll_ctl`,
  `epoll_pwait`, `close`, `pipe2`, `write`.
- `EpollEvent::new` constructor that hides the x86_64 packed vs
  aarch64 padded kernel layout.
- `read_event` helper using `addr_of!` + `read_unaligned` so the
  packed-struct read on x86_64 is sound.
- `Events` container backed by `alloc::vec::Vec` (double-buffered:
  raw kernel events + translated portable events).
- Four conformance tests in `sys::linux::tests`: register-lifecycle,
  pipe-write readability, zero-timeout, short-timeout.
- GitHub Actions CI workflow: fmt → clippy (x86_64 + aarch64) →
  check → doc → test.

## [0.1.0] (checkpoint — not released)

### Added
- Workspace + crate skeleton.
- Public API surface: `Token`, `Interest`, `Ready`, `IoError`,
  `IoErrorKind`, `IoResult`, `Source` trait, `Poll`, `Registry`,
  `Event`, `Events`, `EventsIter`.
