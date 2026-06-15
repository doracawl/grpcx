# sha1-rfc3174 — perf budgets

> Source of truth for performance claims.
>
> Machine: Apple M4 Max (AArch64, SHA crypto extensions).
> Methodology: `comparators/run.sh` — identical timing
> harness in Rust / C / Go (per (op, size): ~100 ms warmup,
> 5 batches of ≥ 10 ms, best-of-batch ns/op), serial
> execution, best of 3 full rounds. Captured 2026-06-10.
> Run-to-run variance on this machine is ±5-8% — treat
> single-digit-percent deltas as a tie.

## Implementation

- **AArch64 hardware path** (`aarch64_sha` module):
  `SHA1C`/`SHA1P`/`SHA1M`/`SHA1H`/`SHA1SU0`/`SHA1SU1`
  intrinsics, software-pipelined (next block's loads issue
  mid-rounds — the same scheduling OpenSSL's
  `sha1-armv8` perlasm uses), block-to-block state kept in
  the FP register file (no GPR round-trips), branchless
  next-block address selection.
- **Portable scalar fallback** everywhere else — also the
  in-tree spec-readable reference and differential-test
  oracle (`sha1_scalar_oracle`, tested at every length
  0..=600 against the dispatched path).

## Cross-language comparison (GiB/s, best of 3 rounds)

| Input | **ours** | OpenSSL 3.x (C) | Go stdlib | community `sha1` 0.10 |
|---|---:|---:|---:|---:|
| 1 KiB | 2.82 | 2.27 | **2.87** | 1.25 |
| 64 KiB | 2.87 | **3.15** | 3.07 | 1.34 |
| 1 MiB | **2.99** | 2.92 | 2.98 | 1.39 |

Standing (2026-06-10):

- **vs community Rust `sha1` (default build): 2.1-2.3×
  faster at every size.** Their default build doesn't
  enable the asm feature; this is the configuration the
  ecosystem actually compiles.
- **vs OpenSSL / Go: photo-finish.** We win 1 MiB, lose
  64 KiB by ~9% to OpenSSL's number (which exceeds their
  own 1 MiB figure — L1-residency effect), and trade
  places with Go at 1 KiB within the noise band. All
  three implementations use the identical hardware
  instructions; the residual spread is scheduling +
  measurement variance.

## History

| Date | Change | 1 MiB throughput |
|---|---|---:|
| 2026-06-10 (v0.1.0) | scalar only | 0.46 GiB/s |
| 2026-06-10 (v0.2.0) | + hw path | 2.75 GiB/s |
| 2026-06-10 (v0.2.0) | + software pipeline + FP-domain E + branchless next-load + one-shot fast path | 2.99 GiB/s |

## Possible next steps (not committed)

- Inline-`asm!` block loop replicating OpenSSL's exact
  instruction placement — could close the remaining 64 KiB
  gap, but the gap is near the noise floor; verify with a
  pinned-frequency measurement environment first.
- x86_64 SHA-NI path — needs hardware to test on; do NOT
  ship untested intrinsics.

## What we would NOT do

- Pull `cpufeatures`-style runtime detection crates — the
  0-dep guarantee stays. Compile-time `target_feature`
  dispatch only (the `sha2` feature is on by default for
  Apple Silicon and ARMv8-A server targets).
