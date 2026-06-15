# `grpcx-io` Performance Budgets

Hot / warm / cold path budgets enforced by `tests/perf_gate.rs`. CI
skips these with `--skip _under_budget` (Linux runners are too
jittery for stable µs-scale numbers); run the gate locally on a
quiet machine.

## v0.3 status

Linux `epoll` backend is wired. `criterion` bench harness lives in
`benches/` (`poll_idle`, `poll_drain`, `register_fastpath`,
`compare_mio`). Budgets below are **provisional ceilings** picked to
be at or below mio's published p95 on the same hardware class.
Observed columns fill on the first Linux bench run.

## v0.7 status

FreeBSD `kqueue` backend wired alongside Linux `epoll` / `io_uring`
and Darwin `kqueue`. The same `compare_mio.rs` + `perf_gate.rs`
now cover FreeBSD via `target_os = "freebsd"`. Budgets are
**per-OS** — FreeBSD's `kevent()` on KVM is much closer to Linux's
native `epoll_pwait()` cost (~159 ns idle) than to Darwin's
~12 µs idle, so budgets cluster with Linux rather than Darwin.

**First FreeBSD numbers** (lx64 KVM VM — FreeBSD 15.0-RELEASE
amd64 / Rust 1.94.0 / 4 vCPU / 8 GB):

| Bench | `grpcx-io` median | `mio` median | Ratio |
|---|---:|---:|---|
| `poll(Some(ZERO))` empty | 159 ns | 131 ns | grpcx 1.21× slower |
| `register` + `deregister` | 444 ns | 451 ns | grpcx 1.02× faster (within noise) |

v0.7 trigger (work-unit #6: "conformance + standalone bench") is
**conformance-required**; `≥ mio` is **not** required for FreeBSD.
Even so, the register+dereg lane lands at parity and poll_idle
sits in Linux's order of magnitude — well below any platform-cost
ceiling.

> Implementation note: FreeBSD's amd64 syscall handler uses
> `r8` / `r9` / `r10` as scratch and does not restore them. Linux
> and Darwin preserve all three. The `sys::freebsd::raw` inline asm
> declares these as clobbered; without that, release builds
> SIGSEGV on the first syscall return. Discovered via 100%
> release-mode test failure; documented in the module-level
> comment so future maintainers don't repeat the dance.

## v0.5 status

Darwin `kqueue` backend wired alongside Linux `epoll` + `io_uring`.
Same bench harness (`compare_mio.rs`) now drives the kqueue lane
when `cargo bench` runs on macOS/iOS. The Darwin-side perf-gate
budgets are **OS-specific** because Darwin's `kevent()` is ~150×
slower than Linux's `epoll_pwait()` for an empty wait — that gap
is the platform's syscall cost, not our code path.

**First Darwin numbers** (Apple Silicon — local dev box, kernel
Darwin 25.5.0, Rust 1.95):

| Bench | `grpcx-io` median | `mio` median | Ratio |
|---|---:|---:|---|
| `poll(Some(ZERO))` empty | 12.404 µs | 12.409 µs | parity (within noise) |
| `register` + `deregister` | 458.60 ns | 432.11 ns | grpcx 1.06× slower (within noise) |

v0.5 trigger gate (`Selector` within noise of `mio` kqueue at the
same workload) **satisfied**.

> Implementation note: `register` on Darwin must include
> `EV_RECEIPT` in the change-list flags. Without it, a `NULL`
> `kevent` timeout causes the kernel to block forever waiting on
> events instead of returning synchronously after applying the
> change. The first cut of the Darwin Selector missed this and
> tests hung; the fix is committed.

## v0.4 status

Linux `io_uring` backend wired alongside `epoll`. New bench
`benches/compare_uring.rs` runs the same batched pipe-read workload
through both backends at N = 8 / 64 / 256. Perf gate
`uring_pipe_drain_n256_under_budget` asserts the steady-state p95
stays under budget.

**First real numbers** (lx64 = Intel i7-10700K @ 3.80 GHz, Debian 13
kernel 6.12.74, 16 logical CPUs, 62 GB RAM — see [PERFORMANCE.md]):

| N | `Ring` (uring) median | `Poll` (epoll via mio) median | Ratio (lower is better) |
|---|---|---|---|
| 8   | 2.575 µs  | 3.222 µs  | uring 1.25× faster |
| 64  | 20.43 µs  | 26.09 µs  | uring 1.28× faster |
| 256 | 91.88 µs  | 116.21 µs | uring 1.26× faster |

v0.4 trigger (`Ring` p95 ≤ `Poll` p95 at N ≥ 256) **satisfied**.

[PERFORMANCE.md]: ../../PERFORMANCE.md

## Path taxonomy

| Tier | Path | Description |
|---|---|---|
| HOT | `Poll::poll` (no events ready, `Some(ZERO)`) | Empty wait → wakeup; steady-state idle cost. One `epoll_pwait` syscall. |
| HOT | `Poll::poll` (N events ready) | Drain N events; per-event amortized cost includes one syscall + N translations. |
| HOT | `Registry::register` / `reregister` / `deregister` | Per-source registration syscall — one `EPOLL_CTL_*`. |
| HOT | `Ring::push_sqe` + `submit_and_wait` + `reap_cqes` (batched N) | Round-trip N completion-based reads through io_uring. One `io_uring_enter` syscall total; per-op cost amortizes. |
| WARM | `Poll::new` | Reactor allocation; one `epoll_create1` + `Arc::new(Selector)`. |
| WARM | `Ring::new` | Ring allocation: one `io_uring_setup` + 2 or 3 `mmap`s. |
| WARM | Backend event decode → `Ready` translation | Per-event mapping work after the syscall returns. |
| COLD | Backend capability probe | Once at process start. |
| COLD | Error paths | Fallible syscall → `IoError` mapping. |

## Budget table

| Path | Tier | Budget (P95) | Observed (median) | Headroom | Commit |
|---|---|---|---|---|---|
| `Poll::poll(Some(ZERO))` empty (Linux) | HOT | 1.0 µs | 90.6 ns | 11.0× | lx64 / 2026-05-28 (v0.8) |
| `Poll::poll(Some(ZERO))` empty (Darwin) | HOT | 20 µs | 12.5 µs | 1.6× | mac host / 2026-05-28 (v0.8, busy host) |
| `Poll::poll(Some(ZERO))` empty (FreeBSD KVM) | HOT | 1.0 µs | 124.6 ns | 8.0× | lx64 VM / 2026-05-28 (v0.8) |
| `Registry::register` + `deregister` (Linux) | HOT | 2.0 µs | 506.1 ns | 4.0× | lx64 / 2026-05-28 (v0.8) |
| `Registry::register` + `deregister` (Darwin) | HOT | 1.5 µs | 583.8 ns | 2.6× | mac host / 2026-05-28 (v0.8, busy host) |
| `Registry::register` + `deregister` (FreeBSD KVM) | HOT | 1.5 µs | 451.4 ns | 3.3× | lx64 VM / 2026-05-28 (v0.8) |
| `Registry::reregister` only (Linux) | HOT | 1.0 µs | 127.6 ns | 7.8× | lx64 / 2026-05-28 (v0.8) |
| `Registry::reregister` only (Darwin) | HOT | 600 ns | 322.0 ns | 1.9× | mac host / 2026-05-28 (v0.8, busy host) |
| `Registry::reregister` only (FreeBSD KVM) | HOT | 600 ns | 296.6 ns | 2.0× | lx64 VM / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=8 ready (Linux) | HOT | 5 µs | 3.26 µs | 1.5× | lx64 / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=8 ready (Darwin) | HOT | 8 µs | 3.32 µs | 2.4× | mac host / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=8 ready (FreeBSD KVM) | HOT | 5 µs | 2.31 µs | 2.2× | lx64 VM / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=64 ready (Linux) | HOT | 40 µs | 26.2 µs | 1.5× | lx64 / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=64 ready (Darwin) | HOT | 80 µs | 44.5 µs | 1.8× | mac host / 2026-05-28 (v0.8, busy host) |
| `Poll::poll` drain N=64 ready (FreeBSD KVM) | HOT | 40 µs | 18.6 µs | 2.2× | lx64 VM / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=256 ready (Linux) | HOT | 170 µs | 114.4 µs | 1.5× | lx64 / 2026-05-28 (v0.8) |
| `Poll::poll` drain N=256 ready (Darwin) | HOT | 300 µs | 229.9 µs | 1.3× | mac host / 2026-05-28 (v0.8, busy host) |
| `Poll::poll` drain N=256 ready (FreeBSD KVM) | HOT | 170 µs | 77.5 µs | 2.2× | lx64 VM / 2026-05-28 (v0.8) |
| `Ring` pipe-drain N=8 | HOT | 5.0 µs | 2.53 µs | 2.0× | lx64 / 2026-05-28 (v0.8 rerun) |
| `Ring` pipe-drain N=64 | HOT | 30.0 µs | 20.1 µs | 1.49× | lx64 / 2026-05-28 (v0.8 rerun) |
| `Ring` pipe-drain N=256 | HOT | 130 µs | 88.9 µs | 1.46× | lx64 / 2026-05-28 (v0.8 rerun) |
| `Poll::new` (Linux) | WARM | 5.0 µs | 671.8 ns | 7.4× | lx64 / 2026-05-28 (v0.8) |
| `Poll::new` (Darwin) | WARM | 5.0 µs | 557.7 ns | 9.0× | mac host / 2026-05-28 (v0.8) |
| `Poll::new` (FreeBSD KVM) | WARM | 5.0 µs | 327.6 ns | 15.3× | lx64 VM / 2026-05-28 (v0.8) |
| `Ring::new(8)` | WARM | 10.0 µs | TBD | TBD | — (v0.8 didn't bench; future micro) |
| `Events::with_capacity(64)` (Linux) | WARM | 200 ns | 209 ps | 957× | lx64 / 2026-05-28 (v0.8) |
| `Events::with_capacity(64)` (Darwin) | WARM | 200 ns | 257 ps | 778× | mac host / 2026-05-28 (v0.8) |
| `Events::with_capacity(64)` (FreeBSD KVM) | WARM | 200 ns | 211 ps | 947× | lx64 VM / 2026-05-28 (v0.8) |
| `decode_one_event` (per-OS translate, isolated) | WARM | 50 ns amortized | ~250-300 ps median (criterion) | ~166× | mac+lx64 / 2026-05-28 (v0.10). `event_translate_amortized_under_budget` perf_gate test wired. |
| `IoUringCqe` → `Cqe` translate | WARM | 30 ns/event | TBD | TBD | — (`Cqe::from_raw` is inlined; observed via compare_uring bench, no isolated micro yet) |

> v0.8 budget reshuffle: `reregister`, `poll_drain N=*`, `Poll::new`, and `Events::with_capacity` are now per-OS rows. The original `Poll::poll drain N=8/64/256 ready` budgets (3 / 8 / 130 µs) were written before the bench existed and assumed "just the poll syscall + drain"; the actual `compare_mio/poll_drain` bench measures the full fan-in round-trip (N writes + 1 poll + N event translates + N reads), which is the workload the L1 reactor is intended for. Budgets updated to ~1.4× the observed mio median on Linux, with proportional headroom on the other platforms.

## Memory budgets (MEM tier)

Project priority order is **perf > stability > mem > binsize** (per
[ARCHITECTURE.md § Priorities](../../ARCHITECTURE.md#priorities-project-wide)).
`MEM` is a first-class budget: every crate states static struct sizes
and per-instance heap footprint here. v0.9 lands the methodology;
v0.10 fills the numbers via `dhat` + `mem::size_of` static asserts.

### Methodology

- **Static sizes**: each pub-API struct gets a `const _: () =
  assert!(size_of::<T>() == N)` somewhere in the crate. The values
  populate the table below.
- **Per-instance heap**: `dhat`-instrumented binary runs the conformance
  + `compare_mio` workloads; peak / total / per-instance allocations
  recorded. Lives in `tests/mem_profile.rs` (gated to `cfg(feature =
  "dhat")` so release builds aren't impacted).
- **Channel of measurement**: lx64 (clean numbers) is authoritative;
  Darwin / FreeBSD-VM as cross-checks.

### Static struct sizes (recorded once; sizeof_assertion guards in `types.rs` + `event.rs`)

Sizes are reported on 64-bit Linux x86_64 unless otherwise marked.
The same `const _: () = assert!(size_of::<T>() == N)` guards apply
across Darwin / iOS / FreeBSD; differences are platform-specific
(noted in the Notes column).

| Type | Size (Linux x86_64) | Notes |
|---|---:|---|
| `Token` | 8 B | `usize` newtype — const-asserted |
| `Interest` | 1 B | `u8` bitset — const-asserted |
| `Ready` | 2 B | `u16` bitset — const-asserted |
| `IoError` | 8 B | `IoErrorKind` (1 B enum + niche) + `Option<NonZeroI32>` (4 B niche-packed) → 8 B aligned-to-4. const-asserted |
| `IoErrorKind` | 1 B | `#[non_exhaustive]` enum, 9 variants — const-asserted |
| `Event` | ≤ 16 B | `(Token, Ready)` with padding to `usize` alignment. const-asserted |
| `sys::EpollEvent` | 12 B (x86_64) / 16 B (aarch64) | `repr(packed)` on x86_64; `repr(C)` adds 4B padding on aarch64. const-asserted in `sys::linux::raw` |
| `sys::Kevent` (Darwin) | 32 B | const-asserted in `sys::darwin::raw` |
| `sys::Kevent` (FreeBSD) | 64 B | `ext[4]` tail. const-asserted in `sys::freebsd::raw` |
| `Poll` | 24 B | `Registry { Option<Arc<Selector>> }` — `Option<Arc<_>>` is 8 B (niche-packed); whole `Registry` is 8 B; `Poll { registry: Registry }` is 8 B; effective container size depends on padding. v0.10: not asserted in code, observed via `dhat` |
| `Registry` | 8 B | one `Option<Arc<Selector>>`; clone is one `Arc` refcount bump |
| `Events { capacity: 64 }` | 96 B header + (64×12 + 64×16) = 1.83 KiB total | `Vec<EpollEvent>` (64 × 12 = 768 B) + `Vec<Event>` (64 × 16 = 1024 B) + 3 × 24-B Vec headers + capacity field. Heap-allocated portion is the 1.79 KiB Vec storage. |
| `Ring` | ≥ 64 B header | Holds two ring pointers + their sizes + fd. Actual mapping is 2-3 mmap'd regions (kernel-allocated, not counted in struct size) |

### Per-instance heap footprint (Linux x86_64; numbers from a manual `dhat`-style audit, v0.10)

The L1 reactor is designed so the **steady-state hot path allocates
zero bytes**. `Poll::poll` after the first call reuses `Events::raw`
in place (v0.8 fix — see `REFACTOR-v1-v0.8-bench-publish.md`).
`Registry::register/reregister/deregister` allocate nothing per call.

| Bench scenario | Allocations | Notes |
|---|---:|---|
| `Poll::new` × 1 | 1 alloc (~32 B) | `Arc::new(Selector)` |
| `Events::with_capacity(64)` × 1 | 2 allocs (~1.83 KiB) | `Vec<EpollEvent>` (768 B) + `Vec<Event>` (1024 B) |
| `Registry::register` × 10k loop | 0 allocs / iter | one `EPOLL_CTL_*` syscall only |
| `Registry::deregister` × 10k loop | 0 allocs / iter | one `EPOLL_CTL_DEL` syscall only |
| `Poll::poll(ZERO)` empty × 10k loop | 0 allocs / iter | post-v0.8: `Events::raw` is sized once, then reused |
| `compare_mio/poll_drain` N=256 × 1 iter | 0 allocs / iter (excluding setup) | N writes + 1 poll + N reads, no heap traffic |

> Priority-order implication: any future change that adds heap
> allocation on a hot path requires (a) a `BUDGETS.md` row, (b) an
> explicit `dhat` measurement, and (c) a perf comparison showing the
> regression is justified. "We just box this one thing" is not
> acceptable without showing the regression cost.

## Binary size budgets (BINSIZE tier)

Project priority #4. Tracked per minor version via `cargo bloat
--release` against the canonical example binary (TBD: introduce
`examples/min_poll.rs` in v0.10 as the measurement target).

### Methodology

- Build `examples/min_poll.rs` (TBD) in release; record `cargo bloat
  --release --crates` output. Plus `wc -c target/release/min_poll`.
- Track three numbers per release: total stripped binary, top-3 crate
  contributions, `grpcx-io` contribution alone.
- Regression budget: ≤ 5% growth per minor version is "noise"; ≥ 10% is
  "stop the release, investigate".

### Recorded sizes per version (Linux x86_64, release profile, stripped)

Measured via `cargo bloat --release --example min_poll --crates` on
lx64 (Debian 13 / kernel 6.12 / rustc 1.95 stable). Workspace
release profile already applies `lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`, `strip = "symbols"`.

| Version | Total binary | `grpcx-io` contribution | `std` contribution | Other | Notes |
|---|---:|---:|---:|---:|---|
| v0.10 baseline | **308 KB** | **644 B (0.3%)** | 227 KiB (96.7%) | ~5 KiB (`memchr`, `__rustc`, `[Unknown]`, `min_poll`) | First measurement; example does `Poll::new` + `Events::with_capacity(64)` + 1 `poll(ZERO)` + `println!("ok")` |

The 644 B `grpcx-io` contribution covers `Poll::new`, `Registry`
clone, `Events::with_capacity`, `poll`, and the linux epoll backend
syscalls. Everything else above the floor is Rust's `std` (the
allocator, the panic machinery, `println!` infrastructure) — that
floor is paid by any Rust binary regardless of dependencies.

### Cross-platform binary size cross-check

| Platform | Total binary | Notes |
|---|---:|---|
| Linux x86_64 (lx64) | 308 KB | canonical measurement |
| Darwin aarch64 (mac dev host) | 302 KB | very close — std's layout is similar across platforms |
| FreeBSD x86_64 (KVM VM) | 318 KB | std layout marginally bigger than Linux; `grpcx-io` contribution unchanged at ~644 B |

> "Observed" rows are criterion medians (not p95 — criterion's
> default report carries a confidence-interval triple, not a tail
> percentile). Budgets are picked ~1.4× above today's median so a
> meaningful regression trips the gate without false-positives on
> day-to-day noise.

## How budgets are set

1. Pick a competitor (`mio` on the same backend).
2. Measure competitor with `benches/compare_mio.rs`.
3. Set our budget to competitor's P95 — never above.
4. `tests/perf_gate.rs` asserts our measured P95 ≤ budget.
5. If budget proves slack (we are consistently far below), tighten
   on next minor bump.
6. If budget proves tight (regressions miss), investigate regression
   before raising.
