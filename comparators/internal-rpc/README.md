# internal-rpc-bench — T5.4 perf harness

Exercises every gate in **RFC § 8** of the internal-RPC
v1 architecture. Bench-only crate; never imported by any
runtime `grpcx-*` crate.

## RFC § 8 gates (Linux 6.x reference)

| Gate                                         | Median, 64 B payload |
| -------------------------------------------- | ---: |
| shmem RTT, busy-poll on                      | < 500 ns |
| shmem RTT, park fallback only                | < 3 µs |
| shmem throughput, single conn                | > 5 GB/s |
| UDS RTT                                      | < 5 µs |
| tcp-mux v2 RTT (io_uring + SQPOLL, Linux)    | < 8 µs |
| tcp-mux v2 RTT (kqueue, Darwin)              | < 15 µs |
| TS + N-API + shmem unary RTT                 | < 1 µs |
| TS + N-API + UDS unary RTT                   | < 15 µs |
| grpcx-pb codegen, 1000-method `.proto`       | < 1 s |

The TS-N-API rows live in `ts/packages/grpcx-bench/`;
everything else lives here.

## Build

```
cargo build --release -p internal-rpc-bench
```

## Run

```
# All gates, JSON written to results/<host>-<arch>-<timestamp>.json:
cargo run --release -p internal-rpc-bench -- --gate all

# Single gate:
cargo run --release -p internal-rpc-bench -- --gate shmem-rtt-busy

# Custom sample count / payload:
cargo run --release -p internal-rpc-bench -- --gate uds-rtt --samples 50000 --payload 64
```

CLI surface:

| Flag | Default | Notes |
| ---- | ------- | ----- |
| `--gate <name>` | `all` | One of `all`, `shmem-rtt-busy`, `shmem-rtt-park`, `shmem-throughput`, `uds-rtt`, `tcpmux-rtt`, `codegen-1000m`. |
| `--samples N` | `10000` | RTT samples after warm-up. |
| `--payload N` | `64` | Echo body size for RTT runs. |
| `--throughput-bytes N` | `1_000_000_000` | Bytes per shmem throughput run. |
| `--output FILE` | derived | Override the result-file path. Default: `results/<host>-<arch>-<unix>.json`. |

The driver always emits `results/...json` (stdout repeats
it for human consumption). The JSON includes a hardware
fingerprint header (`hostname`, `os`, `arch`,
`cpu_count`, `cargo_profile`, optional `git_sha`).

## Reference hardware (RFC § 8)

Linux 6.x, 8-core x86_64, `schedtool -F` realtime
priority, `taskset` core-pinned. On other hardware the
absolute numbers shift; the JSON output records what
the box actually was so the comparator stays honest.

## Result files

`results/` is committed. Each file is a JSON object
keyed by gate name. The intent is to track measurements
across hardware so a future bisect can compare cleanly.
Naming convention: `<host>-<arch>-<unix>.json`
(`<unix>` from `SOURCE_DATE_EPOCH` or `BENCH_TIMESTAMP`
env if set, otherwise `0`).
