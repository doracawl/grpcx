# byte-cursor — perf budgets

> Source of truth for performance claims. Numbers below come
> from `cargo bench -p byte-cursor --bench throughput --
> --sample-size 10 --measurement-time 1 --warm-up-time 1`
> on the captured run of 2026-06-10.
>
> No community comparator at this granularity — the cursor
> is sized for a parser inner loop, not as a standalone
> library replacement.

## Throughput (mean point estimate)

| Workload | Time | Throughput |
|---|---:|---:|
| `advance` × 1 MiB | 312 ps (loop hot, optimizer-folded) | (unmeasurable — see caveat) |
| `take_while` runs of `[a-z]` over 1 MiB with `\n` every 80 B | 266 µs | ~3.68 GiB/s |

## Caveat on the `advance` number

The `while cur.advance().is_some() {}` micro-loop in the
bench compiles down to a tight register-only loop that LLVM
can — and does — collapse. `black_box(&buf)` prevents the
*data* from being constant-folded but the cursor state
machine has no observable side effects, so the compiler
proves the entire loop is dead at the bench level.

The 312-picosecond figure is the iteration-counter overhead,
not the cost of `advance`. Treat as: "`advance` does NOT
appear in profiles when used in real parser inner loops."

## Take-away

For lexer / scanner work (the use case that drove the
extraction — `grpcx-pb`'s `.proto` lexer + `grpcx-ws`'s
HTTP/1 extension parser), `take_while` measures at
~3.7 GiB/s — the inner-loop cost is bounded by the
predicate cost, not by the cursor itself.

## What we measure carefully NOT

- `Cursor::new` / `Cursor::from_str` construction — single
  pointer + ints, sub-nanosecond.
- `peek` / `peek_at` — bounds-checked indexed reads, also
  sub-nanosecond.
- `line` / `col` tracking — `\n` branch in `advance`. The
  `take_while` number above includes this.

## Acceptance bound

If a parser using this crate spends > 10% of its time inside
`Cursor` methods (per a profiler), revisit the bench
methodology. As of 2026-06-10, `grpcx-pb`'s lexer doesn't.
