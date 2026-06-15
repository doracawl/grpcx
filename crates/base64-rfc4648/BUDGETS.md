# base64-rfc4648 — perf budgets

> Source of truth for performance claims.
>
> Machine: Apple M4 Max (AArch64).
> Methodology: `comparators/run.sh` — identical timing
> harness in Rust / C / Go (per (op, size): ~100 ms warmup,
> 5 batches of ≥ 10 ms, best-of-batch ns/op), serial
> execution, best of 3 full rounds. Captured 2026-06-10.

## Implementation — NEON lanes (AArch64) + scalar

On AArch64 (the `neon` target feature is on by default),
hot paths run vectorized:

- **Encode** (aklomp/Muła reshuffle): de-interleave 48
  input bytes into 3 byte-planes (`vld3q`), recombine into
  4 six-bit planes, map through the 64-byte alphabet held
  in 4 vector registers (`vqtbl4q`), interleave-store 64
  ASCII bytes (`vst4q`).
- **Decode** (Muła nibble-LUT, standard alphabet):
  classify each byte by (hi, lo) nibble through two
  16-byte masks (AND ≠ 0 ⟺ invalid), translate via per-high-nibble
  delta with the `/` special case folded through an
  equality mask; 64 chars → 48 bytes per group
  (`vld4q` → pack → `vst3q`). Any invalid byte → whole-call
  fallback to scalar for precise error reporting (the
  error-priority contract is preserved; invalid input is
  the cold case). URL-safe alphabet decodes through the
  scalar path.
- Validity masks are differentially verified against the
  scalar decode table for all 256 byte values
  (`neon_invalid_classification_matches_scalar_table`).

Final cross-language standings with the NEON lanes
(GiB/s, serial run, Apple M4 Max, 2026-06-10):

| Op / size | **ours** | community `base64` 0.22 | OpenSSL 3.x | Go stdlib |
|---|---:|---:|---:|---:|
| encode 1 KiB | **18.2** | 3.79 | 3.76 | 1.99 |
| encode 64 KiB | **23.9** | 3.76 | 3.99 | 2.20 |
| encode 1 MiB | **23.4** | 3.99 | 3.93 | 2.45 |
| decode 1 KiB | **11.3** | 3.50 | 2.75 | 2.70 |
| decode 64 KiB | **12.6** | 3.48 | 2.76 | 3.31 |
| decode 1 MiB | **12.0** | 3.73 | 2.87 | 3.39 |

**#1 at every cell: 4.8-6× the next-best on encode,
3.2-3.6× on decode.** The scalar numbers further below
remain the relevant comparison for non-AArch64 targets.

## Scalar implementation (fallback — no SIMD, no platform branches)

- **Encode**: u64 lane — one unaligned 8-byte big-endian
  load covers two 3-byte groups → 8 masked table lookups,
  written through raw pointers into reserved (not
  zero-filled) capacity; `& 0x3F` index masks make every
  table read provably in-range.
- **Decode**: four pre-shifted u32 tables (`D0..D3` map
  each input byte to its 6-bit value already in final
  position — a quad is 4 loads + 3 ORs, zero runtime
  shifts); two-quad lane emits 6 payload bytes per 8-byte
  store; validity accumulated in a sentinel bit (no
  per-quad branch); structural errors take a `#[cold]`
  diagnostic path that preserves the error-priority
  contract (InvalidCharacter > InvalidLength /
  MissingPadding > NonCanonical).
- `unsafe` is confined to the raw output writes +
  `String::from_utf8_unchecked` over table-produced ASCII,
  each with slice-bounds SAFETY arguments. RFC 4648 § 3.5
  canonical-form rejection is fully preserved (KAT-locked).

## Cross-language comparison (GiB/s, best of 3 rounds)

### Encode

| Input | **ours** | OpenSSL 3.x (C) | community `base64` 0.22 | Go stdlib |
|---|---:|---:|---:|---:|
| 1 KiB | **4.23** | 3.74 | 3.75 | 2.24 |
| 64 KiB | **4.60** | 4.01 | 3.94 | 2.10 |
| 1 MiB | **4.47** | 3.88 | 4.09 | 2.21 |

### Decode

| Input | **ours** | community `base64` 0.22 | Go stdlib | OpenSSL 3.x (C) |
|---|---:|---:|---:|---:|
| 1 KiB | **3.89** | 3.64 | 2.69 | 2.85 |
| 64 KiB | **3.89** | 3.58 | 2.96 | 2.80 |
| 1 MiB | **4.10** | 3.78 | 3.01 | 2.88 |

**Standing (2026-06-10): #1 at every (op, size) cell** —
ahead of the community `base64` crate by 7-17%, OpenSSL by
13-42%, Go stdlib by 36-119%.

## History

| Date | Change | 1 MiB enc / dec |
|---|---|---:|
| 2026-06-10 (v0.1.0) | String::push(char) + two-pass decode | 2.06 / 1.33 GiB/s |
| 2026-06-10 (v0.2.0) | byte-buffer writes, single-pass decode | 3.57 / 2.4 GiB/s |
| 2026-06-10 (v0.2.0) | no zero-fill, u64 encode lane, pre-shifted decode tables, two-quad store | 4.47 / 4.10 GiB/s |

## What we would NOT do

- NEON/SSE SIMD paths (à la `base64-simd`): the scalar
  implementation already leads every comparator in scope;
  SIMD would buy headroom we don't currently need at the
  cost of per-platform `unsafe` surface. Revisit only if a
  comparator overtakes us.
- Pull in `memchr`/`hashbrown`-style helper deps — the
  0-dep guarantee stays.
