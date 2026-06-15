# `grpcx-rt` Performance Budgets

Hot / warm / cold path budgets, enforced by `tests/perf_gate.rs` once
backends land (perf_gate wiring deferred to v0.3+, see
[../../docs/v2-scope.md](../../docs/v2-scope.md)). CI skips these with
`--skip _under_budget`; run locally on a quiet machine.

Project priority order is **perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
The competitor lane is `tokio` (`compare_tokio.rs`, lands v0.5).

## v0.1 status

API surface only — no implementation, so **no observed numbers yet**.
This file fixes the path taxonomy so each v0.x checkpoint fills its
rows as it lands. All `Observed` / `Headroom` cells are TBD until the
corresponding backend exists.

## Path taxonomy

| Tier | Path | Lands | Description |
|---|---|---|---|
| HOT | `Spawn::spawn` | v0.2 / v0.5 | Enqueue a task — slab slot reuse, no per-spawn heap alloc. |
| HOT | executor poll loop (per-task `poll`) | v0.2 | Per-ready-task wake → `poll` dispatch; the steady-state scheduler cost. |
| HOT | `Waker::wake` | v0.2 | Mark a task runnable via custom `RawWakerVTable` → slab task id. |
| HOT | `block_on` park / unpark | v0.2 | One `grpcx-io::Poll::poll` round-trip per idle cycle (no condvar). |
| HOT | `Timer` insert / fire | v0.3 | O(1) hierarchical timing-wheel slot insert + fire. |
| HOT | `Sender::try_send` / `Receiver::try_recv` | v0.4 | One ring-buffer slot write / read; `Err(value)` on full, no alloc. |
| HOT | work-steal (`multi_thread`) | v0.5 | Lock-free local-queue pop + cross-worker steal. |
| WARM | executor / runtime construction | v0.2 / v0.5 | Worker thread spawn + slab pool + reactor wiring (once per runtime). |
| WARM | channel construction (`channel(cap)`) | v0.4 | One fixed-capacity ring-buffer allocation per channel. |
| COLD | runtime shutdown / drain | v0.2+ | Join workers, drain pending tasks. |
| COLD | error paths | v0.2+ | Pool exhaustion / channel-full / closed → typed `Err`. |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| (all rows) | — | TBD | TBD | TBD | — (v0.1 is API-only; filled per checkpoint vs `compare_tokio`) |

## Memory budgets (MEM tier)

Per-task footprint, slab pool sizing, channel ring footprint, and
per-instance heap. Methodology mirrors `grpcx-io/BUDGETS.md` (static
`size_of` const-asserts + a `dhat`-gated profile). Numbers land v0.6.

Observed on the dev host (macOS aarch64) at v0.5 via
`runtime::sizes::report_internal_sizes` + the `const _` size asserts in
that module. Heap-payload types (`Task`, `Shared`) embed std `Mutex` /
`Condvar`, so their size is platform-dependent and is recorded, not
const-asserted; the public handles are a single `Arc` pointer and **are**
const-asserted (`== size_of::<usize>()`).

| Type | Size | Notes |
|---|---:|---|
| `RtHandle` | 8 B | `Arc<Shared>`; const-asserted (1 ptr) |
| `JoinHandle<T>` | 8 B | `Arc<JoinShared<T>>`; const-asserted (1 ptr) |
| `Sender<T>` / `Receiver<T>` | 8 B / 8 B | `Arc<Chan<T>>`; const-asserted (1 ptr) |
| `Task` (MT) | 48 B | `Arc`-shared unit: `Mutex<Option<BoxFuture>>` + `AtomicU8` state + `Arc<Shared>`. One heap node per spawn. |
| `Shared` (MT) | 9496 B | One per `Runtime`. Dominated by the inline 6×64 timer `Wheel`; not a per-task cost (warm/once-per-runtime). |

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via `cargo bloat
--release --example min_spawn --crates` against the canonical
`examples/min_spawn.rs` (construct a `Runtime`, `block_on` one spawned
task). Regression budget: ≤ 5% growth per minor is noise; ≥ 10% stops
the release.

| Version | Total binary | `grpcx-rt` contribution | Notes |
|---|---:|---:|---|
| v0.1 | — | — | API-only; no example binary yet |
| v0.5 | 332 KiB | 12.7 KiB `.text` | macOS aarch64, `release` (fat LTO, stripped). `min_spawn` `.text` 232.9 KiB total: std 213.5 KiB, grpcx-rt 12.7 KiB (5.4%), grpcx-io 1.1 KiB. The runtime's own footprint is small; std dominates a std-backed binary. |
