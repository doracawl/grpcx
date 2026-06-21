# @grpcx/bench — T5.4 TS-side bench harness

Drives the two TS + N-API gates from RFC § 8:

| Gate                          | Median, 64-byte payload |
| ----------------------------- | ---: |
| TS + N-API + shmem unary RTT  | < 1 µs |
| TS + N-API + UDS unary RTT    | < 15 µs |

The other seven gates (shmem / UDS / tcp-mux RTT &
throughput, codegen speed) live in the Rust driver at
`comparators/internal-rpc/`.

## Build & run

Rust example servers must already be built:

```
cd <repo-root>
cargo build --release --features grpcx/transport-shmem,grpcx/transport-tcp-mux --examples
```

Then from `ts/`:

```
npm run build --workspaces --if-present
node packages/grpcx-bench/src-ts/index.mjs --gate all
```

CLI surface:

| Flag | Default | Notes |
| ---- | ------- | ----- |
| `--gate <name>` | `all` | `all`, `shmem-rtt`, `uds-rtt`. |
| `--samples N` | `10000` | RTT samples after warm-up. |
| `--payload N` | `64` | Echo body bytes. |
| `--output FILE` | derived | JSON result path. Default: `results/<host>-<arch>-<unix>.json`. |

Set `BENCH_TIMESTAMP=$(date +%s)` so the output file
includes a monotonic timestamp.

## Reference hardware (RFC § 8)

Linux 6.x, 8-core x86_64, `schedtool -F` realtime,
`taskset` core-pinned. Other hardware shifts the
absolute numbers — the JSON header records what the
box actually was.
