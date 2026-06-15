# grpcx-rt

[![crates.io](https://img.shields.io/crates/v/grpcx-rt.svg)](https://crates.io/crates/grpcx-rt)
[![docs.rs](https://docs.rs/grpcx-rt/badge.svg)](https://docs.rs/grpcx-rt)
[![license](https://img.shields.io/crates/l/grpcx-rt.svg)](https://github.com/goliajp/grpcx)

Zero-dep async runtime — task scheduler + reactor + static-layer timer
wheel + fixed-size ring channels, built on [`grpcx-io`](../grpcx-io).
The `tokio`-class layer of the grpcx stack (L2). No `libc`, no `mio`,
no `tokio`.

## Status (v1.0 — L2 closed, semver-stable)

L2 deliverable complete (project v2.0, released 2026-06-07). Stable
surface: the `current_thread` executor (`block_on`, `spawn` /
`JoinHandle` / `Spawn`, the `readable` reactor primitive), the full
time API (`sleep` / `Sleep`, `Timer`, `timeout` / `Timeout` /
`Elapsed`, `interval` / `Interval`), channels (`channel` / `Sender`
/ `Receiver` mpsc + `oneshot`, fixed-size ring), and the
**multi-thread work-stealing `Runtime`** (`Runtime` / `RtHandle` /
`yield_now`): a worker pool over a lock-free Chase-Lev deque +
injector, with a dedicated reactor/timer driver thread and a
cooperative budget that keeps a busy task from starving its peers.
The shared `sleep` / `readable` primitives dispatch to whichever
runtime is current. **Cross-thread channel sends unpark the driver**
via `grpcx_io::Waker`.

Stabilisation evidence (v0.6): 95.10 % line coverage on the dev
host; **1.568 B fuzz executions** in 24 CPU-h cumulative across four
targets, zero crash / leak / OOM; **16 h+ stress soak** clean against
the fixed code (the soak surfaced and closed a real
`Chan::try_recv` TOCTOU race in ≈ 1 h on the first run; the fix +
deterministic regression test landed before re-running). See
[`CHANGELOG.md`](./CHANGELOG.md) for the per-checkpoint trail and
[`../../REFACTOR-v2-v0.7-close-out.md`](../../REFACTOR-v2-v0.7-close-out.md)
for the L2 close-out.

Stability promise: v1.x patch / minor releases preserve the public
surface above. Breaking changes require a `grpcx-rt` v2.0. The
`#[doc(hidden)]` `__fuzz_helpers` module is explicitly **not** part
of the stable surface — same caveat as `grpcx-io`'s
`__bench_helpers`.

## When to reach for this vs `tokio`

Same job as `tokio`'s runtime — spawn tasks, await IO, sleep, pass
messages — inside a zero-dependency stack where every layer is
in-house and the priority order is **performance > stability > memory
> binary size**. Not drop-in `tokio`-compatible; a from-scratch
runtime over `grpcx-io`.

## Design philosophy

Part of the larger [grpcx](../../README.md) project. Read in order:

1. [ARCHITECTURE.md](../../ARCHITECTURE.md) — stones / cement / layer
   discipline / priority axes
2. [DEPS_AUDIT.md](../../DEPS_AUDIT.md) — why every layer is rewritten
3. [ROADMAP.md](../../ROADMAP.md) — L1 → L10 with per-layer triggers
4. [docs/v2-scope.md](../../docs/v2-scope.md) — L2 scope + 11 work
   units + checkpoints + per-checkpoint design notes

## License

Apache-2.0 OR MIT
