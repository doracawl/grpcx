# http1-syntax — perf budgets

> Source of truth for performance claims. Numbers below come
> from `cargo bench -p http1-syntax --bench throughput --
> --sample-size 10 --measurement-time 1 --warm-up-time 1`
> on the captured run of 2026-06-10.
>
> No community comparator at this granularity — `httparse`
> does full HTTP/1 messages, not isolated header values.

## Throughput (mean point estimate)

| Workload | Time | Notes |
|---|---:|---|
| `read_token` × 4 + `read_quoted_string` × 1 on a 140-byte ext header | 47 ns | ~2.93 GiB/s; mixed code paths |
| `token_list_contains` hit (5-element list, target at end) | 80 ns | ~390 MiB/s; comma-walk + token compare |
| `token_list_contains` miss (5-element list, no match) | 80 ns | ~390 MiB/s; same code path as hit |

## Take-away

For the WebSocket handshake use case (one
`Sec-WebSocket-Extensions` header per connection), a single
parse pass is < 100 ns — performance is **not** the
bottleneck.

The token-list lookup walks the list linearly; for the
typical 3-5 element header values in HTTP/1.1 (RFC 7230's
`Connection: keep-alive, Upgrade` style), linear is
strictly faster than hashing.

## Acceptance bound

If parsing a single header value crosses 1 µs (= 20× the
current cost), revisit the bench methodology.

## What we would NOT do

- Pre-allocating a `HashMap` for token-list membership
  testing: at typical list sizes (≤ 5), the linear scan
  wins, and pulling `hashbrown` would break the 0-runtime-
  dep guarantee anyway.
- Adding SIMD for token-character classification: scalar
  table-lookup is already inlined down to a single CMP +
  conditional move in release builds.
