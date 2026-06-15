# Changelog

All notable changes to `grpcx-rt` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.0.0] below)

## [1.0.0] - 2026-06-07

**L2 closed; semver-stable.** Project v2.0 release — `grpcx-rt` joins
`grpcx-io` on the v1.x line. The v0.6 stabilisation + v0.7 close-out
together meet the lab-internal v2 trigger; this release is the
SemVer anchor. See
[../../REFACTOR-v2-v0.7-close-out.md](../../REFACTOR-v2-v0.7-close-out.md).

### Added
- `crates/grpcx-rt/benches/compare_tokio.rs` — head-to-head bench
  harness against `tokio` (eight benches in two groups:
  `current_thread/` and `multi_thread/`). `tokio` enters the
  workspace as a `dev-dependencies` only — the
  zero-external-runtime-dep promise for `src/` stands.

### Stability promise
- `Runtime`, `RtHandle`, `Handle`, `JoinHandle`, `Spawn`,
  `Sender`/`Receiver`/`Recv`/`TrySendError`/`TryRecvError`/`channel`,
  `oneshot::{Sender,Receiver,Canceled,channel}`,
  `Sleep`/`sleep`/`Timer`/`Timeout`/`timeout`/`Elapsed`/`Interval`/
  `interval`, `Readable`/`readable`, `YieldNow`/`yield_now`,
  `block_on`, `spawn`.
- The `#[doc(hidden)]` `__fuzz_helpers` module is **not** part of the
  stable surface — same caveat as `grpcx-io`'s `__bench_helpers`.

### Deferred (post-v1.0)
- `compare_tokio` cross-OS publish + `PERFORMANCE.md` consolidation
  → v1.x perf pass (bench harness already in place; only the
  measurement publication is deferred).
- `tests/perf_gate.rs` regression gate → v1.x perf pass.
- Driver-handoff (workers rotate reactor ownership) + finer
  worker-park / notify heuristics → v1.x perf pass.
- External Rust-experienced reviewer pass → post-external-window.
- ≥ 1 internal lab project on `grpcx-rt` for 1 month → post-host-project.

## [0.6.0] - 2026-06-07

Stabilisation pass — coverage ≥ 95 %, 24 h cumulative fuzz, 24 h
stress soak, MEM/BINSIZE re-measure. The soak surfaced and fixed a
real v0.5 race in `Chan::try_recv`. See
[../../REFACTOR-v2-v0.6-coverage-fuzz-mem.md](../../REFACTOR-v2-v0.6-coverage-fuzz-mem.md).

### Fixed
- **`channel::Chan::try_recv` TOCTOU race** (caught by the v0.6 soak
  at ~0.4 ppb under MT). Between the first `buf.lock().pop()`
  returning `None` and the subsequent `senders.load(Acquire)`, any
  number of still-live senders could each push a value and then drop
  (the last drop's `fetch_sub(AcqRel)` taking `senders` to 0). The
  receiver's load would then see 0 → return `Disconnected`, leaving
  the pushed values stranded in the ring forever. The fix is a
  second `buf.lock().pop()` after observing `senders == 0`; the
  Acquire chain on `fetch_sub` guarantees the second pop sees any
  push that made it into the race window. A deterministic regression
  test (`channel::tests::mpsc_no_disconnected_with_pending_value_race`,
  10 000 rounds × 4 senders) fails without the fix in ≈ 1.5 s on
  lx64.

### Added
- `__fuzz_helpers::drive_channel` + `__fuzz_helpers::drive_runtime`
  byte-driven harnesses; `fuzz_targets/channel_ops.rs` +
  `fuzz_targets/runtime_spawn.rs` over them.
- `tests/stress.rs::stress_soak` (`#[ignore]`d, time-boxed via
  `GRPCX_STRESS_SECS`) — the soak harness whose first ≈ 1 h run
  surfaced the channel race.
- `executor::tests::elapsed_display_and_error_impls` covering
  `Elapsed`'s `Display` + `Error` impls.

### Stabilisation evidence
- Coverage: workspace all-in **95.10 % lines / 94.90 % regions / 98.31 % functions** on the dev host (macOS aarch64).
- Fuzz: **1 567 926 448 executions in 24 CPU-h cumulative across 4
  targets**, zero crash / leak / OOM on lx64 (`fuzz/artifacts/*/`
  empty).
- Stress soak: 16 h 21 m clean on lx64 against the fixed code (user
  sign-off at 17 h vs the pre-fix 1 h 05 m failure window → ≥ 16 ×
  safety margin); soak left to natural 24 h deadline as bonus margin.
- MEM / BINSIZE: no drift vs v0.5 baseline (`size_of::<Task>()` 48 B,
  `size_of::<Shared>()` 9496 B, `min_spawn` `.text` grpcx-rt
  contribution 12.7 KiB).

## [0.5.0] - 2026-05-29

Multi-thread work-stealing runtime (design intent #4) — a worker pool
over the lock-free Chase-Lev deque plus a dedicated reactor/timer driver
thread, on a separate track from the v0.2 `current_thread` executor. See
[../../REFACTOR-v2-v0.5-multithread-executor.md](../../REFACTOR-v2-v0.5-multithread-executor.md).

### Added
- `runtime` module (std-backed): `Runtime` (`new(workers)`, `block_on`,
  `spawn`, `handle`), `RtHandle` (`Clone`, `Send`, `impl Spawn`), and
  `yield_now` / `YieldNow`. Each worker owns a lock-free Chase-Lev
  `Deque<Arc<Task>>` (LIFO local, FIFO steal); a `Mutex<VecDeque>`
  injector takes overflow and off-worker spawns; a dedicated **driver
  thread** owns the reactor + timer wheel and is unparked from its
  blocking `Poll::poll` by `grpcx_io::Waker` when a worker registers
  fresh timer/IO interest.
- `Task` wake state machine (`AtomicU8`:
  `IDLE`/`QUEUED`/`RUNNING`/`RUNNING_NOTIFIED`) with a hand-rolled
  `RawWakerVTable` over `Arc<Task>`, folding a mid-poll wake into a
  single re-schedule. `block_on` runs the root as a spawned task and
  blocks the calling thread on a `Condvar` slot (no worker occupied).
- **Cooperative budget** + `yield_now`: a per-run poll budget (128) that
  forces a busy task to yield so it cannot starve its worker's peers;
  wired into channel `recv`. A no-op outside a multi-thread worker.
- Leaf dispatch: the shared `sleep` / `readable` primitives register
  against the multi-thread runtime's shared timer/reactor when polled on
  a worker, falling back to the current-thread runtime otherwise — one
  user-facing API across both executors.
- 10 runtime integration tests (`tests/runtime.rs`): spawn/join, nested
  spawn, cross-worker mpsc fan-out/in and ping-pong, `sleep` and
  `readable` over MT, a 50 000-task medium stress run, and a
  single-worker cooperative-budget starvation check.

### Changed
- `JoinShared` / `JoinHandle` visibility relaxed to `pub(crate)` (+ a
  `JoinHandle::new`) so both executors share one join type. The
  `current_thread` executor's `block_on` / scheduler / slab are
  otherwise untouched; `Sleep` / `Readable` gained a runtime-dispatch
  hook whose current-thread path is unchanged.
- Crate version `0.4.0` → `0.5.0`.

### Notes
- A re-woken task re-enters via the **injector** (back of the line), not
  the LIFO local deque — a local re-push is popped again immediately and
  starves peers (the `yield_now` / hot-`recv` fairness path).
- Worker park is `Mutex<bool> + Condvar` with cascading `notify_one`;
  correct (lost-wakeup safe) but coarse — finer wake heuristics and a
  driver-handoff alternative to the dedicated driver are deferred to the
  v0.7 perf window. 24h stress is v0.6; v0.5 requires functional
  correctness + a medium stress run only.

### Validated
- macOS aarch64 43 ✅ (weak memory) · Linux x86_64 (lx64) 43 ✅ · FreeBSD
  15.0 amd64 (lx64 KVM) 43 ✅ — lib 33 + runtime 10 each, MT suite looped
  10× debug + release with no flakiness on aarch64.
- Gate green: `fmt --check --all`, `clippy --workspace --all-targets`,
  `doc --workspace`, cross `x86_64-unknown-freebsd` +
  `aarch64-unknown-linux-gnu`, no grpcx-io regression (34 + 3 waker).

## [0.4.0] - 2026-05-29

Channels (`mpsc` + `oneshot`, fixed-size ring) + cross-thread unpark via
`grpcx_io::Waker`. First checkpoint that works across threads. See
[../../REFACTOR-v2-v0.4-channels.md](../../REFACTOR-v2-v0.4-channels.md).

### Added
- `channel` module: bounded **mpsc** (`channel<T>(cap)`, `Sender`
  (`Clone`, non-blocking `try_send` → `TrySendError::Full/Closed`),
  `Receiver` (`try_recv` → `TryRecvError::Empty/Disconnected`, async
  `recv` → `Option<T>`), `Recv`) + **oneshot** (`oneshot::channel`,
  `Sender::send`, `Receiver: Future<Output = Result<T, Canceled>>`).
  Ring-backed (design intent #2): full → value returned, no alloc.
- Cross-thread unpark: `Sched` holds a `grpcx_io::Waker` (registered at
  a reserved token) + the runtime `ThreadId`; a wake from another thread
  fires the unparker to interrupt `block_on`'s blocked `Poll::poll`.
- 9 channel tests incl. **cross-thread** `mpsc_cross_thread_wake` and
  `oneshot_cross_thread` (exercise the per-OS `Waker` unpark).

### Changed
- `channel` / `Sender` / `Receiver` are now real (were `unimplemented!()`
  stubs in 0.3.0). `Sender::try_send` returns `TrySendError<T>` (not the
  bare `Err(T)` of the stub); `Receiver::recv` is async.
- **Removed the stall panic.** A future awaiting a cross-thread wake now
  blocks on `poll(None)` until it arrives (tokio-like), instead of
  panicking on "no runnable task + no io + no timer".
- Crate version `0.3.0` → `0.4.0`.

### Validated
- Linux x86_64 (lx64) 28 ✅ · macOS aarch64 28 ✅ · FreeBSD 15.0 amd64
  (lx64 KVM) 28 ✅. Cross-thread tests confirm the `grpcx_io::Waker`
  unpark on eventfd (Linux) and `EVFILT_USER` (BSD).

## [0.3.0] - 2026-05-29

Static-layer hierarchical timing wheel (design intent #3) + the time
API. See
[../../REFACTOR-v2-v0.3-timer-wheel.md](../../REFACTOR-v2-v0.3-timer-wheel.md).

### Added
- `timer` module (`no_std`): a 6-level × 64-slot hierarchical timing
  wheel. O(1) insert, amortized-O(1) cascade firing, O(levels)
  next-deadline. Tick-agnostic (1 tick = 1 ms mapped by the executor).
- Time API: `Sleep` / `sleep`, `Timer::after`, `Timeout<F>` /
  `timeout` / `Elapsed` (`impl Display + core::error::Error`),
  `Interval` / `interval`.
- `block_on` now blocks on `Poll::poll(Some(timeout))` computed from
  the next timer deadline, so time-based futures wake without IO.
- 7 wheel unit tests (synthetic ticks, deterministic — incl. multi-step
  cross-level cascade) + 6 executor time tests over real time.

### Changed
- `Sleep` / `sleep` / `Timer` are now real implementations (were
  `unimplemented!()` stubs in 0.2.0).
- Crate version `0.2.0` → `0.3.0`.

### Notes
- 1 ms granularity; `sleep` rounds up so it never wakes early (measured
  from runtime start). `Interval` is relative (slow consumer shifts
  later ticks). Dropped `Sleep`s leave a harmless dead wheel entry
  (implicit cancellation).
- Still **same-thread wakeups only** (carried from v0.2). `channel` /
  `Sender` / `Receiver` remain stubbed (v0.4).
- Trigger gate green: workspace `check`/`clippy`/`doc`/`fmt`; cross
  `x86_64-unknown-freebsd` + `aarch64-unknown-linux-gnu`; no grpcx-io
  regression (34 conformance + 2 doctest).

## [0.2.0] - 2026-05-29

L2 first runnable runtime: single-thread `current_thread` executor +
`block_on` + reactor (`Waker` ↔ `grpcx-io::Poll`) integration. See
[../../REFACTOR-v2-v0.2-current-thread-executor.md](../../REFACTOR-v2-v0.2-current-thread-executor.md).

### Added
- `executor` module (std-backed): the real `block_on`, `spawn`,
  `Handle` (`impl Spawn`), `JoinHandle<T>` (`impl Future`), and the
  `readable(fd) -> Readable` reactor-readiness primitive.
- Hand-rolled `RawWakerVTable` over `Arc<WakerData>` (design intent
  #7); task `Slab` (`Vec<Slot>` + free list); `thread_local!` current
  runtime handle; one-shot reactor registration normalizing epoll's
  level-triggered vs kqueue's edge-triggered semantics.
- 6 conformance tests, including `reactor_readable_wakes_future` which
  drives a real `std::io::pipe` through `Poll`.

### Changed
- `block_on` / `Spawn` / `JoinHandle` are now real implementations
  (were `unimplemented!()` stubs in 0.1.0).
- Crate version `0.1.0` → `0.2.0` (L2's own cadence; independent of the
  workspace `1.0.0`).

### Removed
- The public `Task` placeholder struct — it is now an internal slab
  cell, not part of the API surface.

### Notes
- v0.2 supports **same-thread wakeups only**; a self-pipe unparker for
  cross-thread wakes lands with channels (v0.4) / the multi-thread
  executor (v0.5). `Sleep` / `Timer` / `channel` / `Sender` /
  `Receiver` remain stubbed.
- Trigger gate green: workspace `check`/`clippy`/`doc`/`fmt`; cross
  `x86_64-unknown-freebsd` + `aarch64-unknown-linux-gnu`; no grpcx-io
  regression (34 conformance + 2 doctest).

## [0.1.0] - 2026-05-29

L2 bootstrap. Freezes the **public API shape** for the async runtime;
no implementation yet. Opens the v2.x series after L1 (`grpcx-io`)
closed at v1.0 the same day. See
[../../REFACTOR-v2-v0.1-L2-bootstrap.md](../../REFACTOR-v2-v0.1-L2-bootstrap.md).

### Added
- Crate skeleton: `Cargo.toml` (full published metadata, literal
  `version = "0.1.0"` independent of the workspace `1.0.0`, sole dep
  `grpcx-io` via path), `README.md`, this `CHANGELOG.md`, `BUDGETS.md`.
- `#![no_std]` + `extern crate alloc` + the three-line doc gate
  (`include_str!` README, `deny(missing_docs)`,
  `deny(rustdoc::broken_intra_doc_links)`).
- ~8 public API stub items, bodies `unimplemented!()`:
  - `Spawn` trait — the shared seam for both executors.
  - `JoinHandle<T>` — awaitable handle (`impl Future`).
  - `Task` — the slab-pooled unit of scheduling.
  - `block_on` — single-thread driver (over `grpcx-io::Poll`).
  - `sleep` / `Sleep` — timer-wheel one-shot (`impl Future`).
  - `Timer` — timer-wheel driver handle (`Timer::after`).
  - `channel` / `Sender<T>` / `Receiver<T>` — fixed-size ring
    `mpsc` (`try_send` → `Err(value)` backpressure, `try_recv`).
- Eight charter-level design decisions locked in doc comments
  (slab-pool tasks; fixed-size ring channels; static-layer timer
  wheel; dual-track work-stealing scheduler; scoped `no_std`;
  monomorphization over `dyn`; `core::task::Waker` + custom
  `RawWakerVTable`; `block_on` via `Poll`, not condvar). Full
  rationale in `REFACTOR-v2-v0.1-L2-bootstrap.md`.

### Notes
- v0.1 trigger gate green: `cargo check`/`clippy`/`doc`/`fmt` clean on
  the workspace; cross-compile clean on `x86_64-unknown-freebsd` and
  `aarch64-unknown-linux-gnu`.
- Every public item panics if called. This is intentional for an
  API-surface-only checkpoint; implementations land v0.2 onward.
