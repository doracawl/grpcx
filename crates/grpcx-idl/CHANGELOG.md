# Changelog

All notable changes to `grpcx-idl` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.0.0] below)

## [1.0.0] - 2026-06-09

**Project v7.0 release** — L7 closes. Final stabilisation drop:
fuzz harness in place, `sizes.rs` const-asserts +
`report_internal_sizes`, MEM + BINSIZE numbers in `BUDGETS.md`,
coverage ≥ 95 % (96.75 % lines). See
[../../REFACTOR-v7-v0.7-close-out.md](../../REFACTOR-v7-v0.7-close-out.md)
and [../../REFACTOR-v7.0-release.md](../../REFACTOR-v7.0-release.md).

### Added
- `src/sizes.rs` — `size_of` const-asserts on public ADTs
  (Error / tuple archives / Option / Result variants) +
  `report_internal_sizes` test for BUDGETS column.
- `src/__fuzz_helpers.rs` (`#[doc(hidden)]`) — byte-driven
  drivers (`drive_primitive`, `drive_composite`,
  `drive_varlen`, `drive_tagged`).
- `fuzz/` cargo-fuzz subcrate with 4 targets. All build clean
  on nightly + smoke-run zero crashes (~40 M execs cumulative).
- `examples/min_idl.rs` — round-trip example exercising every
  major wire shape (primitive / composite / varlen / tagged /
  derived). Drives BINSIZE measurement.
- `tests/kat_deserialize.rs` — 9 cases covering Deserialize
  paths across all archived types + tuple arities 4-7 +
  accessor edges (`is_empty` / empty String / empty Vec).

### Gate
- `cargo test -p grpcx-idl`: 106 / 106 passing (was 95 / 95 at
  v0.6 close; +9 deserialize KAT + 2 sizes test).
- `cargo test --workspace --skip _under_budget`: 520 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.
- Coverage: `cargo llvm-cov -p grpcx-idl --summary-only
  --ignore-filename-regex '__fuzz_helpers\.rs'` reports
  **96.75 % lines / 100 % fn / 92.62 % regions** — over the
  95 % gate.
- Fuzz: 4 / 4 targets build clean (aarch64-darwin native);
  9.7-10.5 M execs each on the smoke pass, zero crashes.

### Deferred to v1.x
- Cross-OS `compare_rkyv` publish + `PERFORMANCE.md` — same
  charter as L1-L6 (harness lands in perf pass, numbers later).
  rkyv has a complex generic API; an apples-to-apples codec-
  only bench needs care.
- 24 h cumulative fuzz long-run.
- Tuple structs / multi-field tuple variants / named-field
  enum variants in `#[derive(GrpcxIdl)]` — currently reject
  at the macro level.
- Lifetime generic params (`Archive::Archived` can't carry a
  lifetime; ergonomics revisit via an `ArchiveRef` companion
  trait in v1.x).
- `#[grpcx_idl(skip)]` field attribute.
- `#[repr(u8)]` discriminant compaction for ≤ 256-variant enums.

## [0.6.0] - 2026-06-09

**`#[derive(GrpcxIdl)]` proc-macro lands** via the new
`grpcx-idl-derive` sibling crate. Auto-generates
`Archive` / `Serialize` / `Deserialize` impls for user-defined
structs (named fields) + enums (unit variants + single-payload
tuple variants). The macro emits exactly the shape the v0.2-v0.5
hand-rolled impls produce. See
[../../REFACTOR-v7-v0.6-derive.md](../../REFACTOR-v7-v0.6-derive.md).

### Added
- `grpcx-idl-derive` 0.1.0 — new workspace member, `proc-macro = true`:
  - `src/lib.rs` — `#[proc_macro_derive(GrpcxIdl)]` entry,
    dispatches by `syn::Data` shape, rejects lifetime generics
    / unions / unit structs / tuple structs / multi-payload
    variants with clear compile errors.
  - `src/struct_impl.rs` — codegen for named-field structs.
  - `src/enum_impl.rs` — codegen for enums (unit + single-
    payload tuple variants).
- Workspace `Cargo.toml` adds `syn` / `quote` / `proc-macro2`
  as workspace dev-dependencies (compile-time-only tooling
  exception, same class as `criterion` / `rcgen` / `quinn-proto`).
- `grpcx-idl/Cargo.toml` adds `grpcx-idl-derive` as a dev-dep
  for the derive KAT tests.

### Tests
- `crates/grpcx-idl/tests/kat_derive_struct.rs` — 7 cases:
  - 4 round-trips: `Point { x: u32, y: u32 }`,
    `Mixed { a: u8, b: u32, c: bool }` (padding),
    `Wrap { inner: (u32, u8) }` (nested composite), plus a
    Deserialize round-trip.
  - 3 reject paths: truncated / oversize / inner-bool.
- `crates/grpcx-idl/tests/kat_derive_enum.rs` — 7 cases:
  - 5 round-trips: `Status::Idle` / `Running(u32)` / `Done`
    (mixed unit + payload), `Wrapped::Pair((u32, u8))` /
    `Empty`.
  - 2 reject paths: bad disc / truncated.

### Gate
- `cargo build -p grpcx-idl-derive`: clean.
- `cargo test -p grpcx-idl`: 95 / 95 passing (was 81 / 81 at
  v0.5 close; +14 derive KAT).
- `cargo test --workspace --skip _under_budget`: 509 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.5.0] - 2026-06-09

**Tagged unions — `Option<T>` + `Result<T, E>`.** Fifth L7
checkpoint. Locks the wire format for enums with payloads;
user-defined enums wait for v0.6's derive macro. See
[../../REFACTOR-v7-v0.5-tagged-unions.md](../../REFACTOR-v7-v0.5-tagged-unions.md).

### Added
- `src/tagged.rs`:
  - `#[repr(u32)] pub enum ArchivedOption<T>` where
    `T: Archive, T::Archived: Sized`. Variants `None = 0` /
    `Some(T::Archived) = 1`.
  - `#[repr(u32)] pub enum ArchivedResult<T, E>` with
    `Ok = 0` / `Err = 1`.
  - `Archive` / `Serialize<W>` / `Deserialize<D>` for
    `Option<T>` and `Result<T, E>`.
  - `pad_with_zeros` helper for the explicit-byte-padding
    encoder path.
- `lib.rs` — re-exports `ArchivedOption` + `ArchivedResult`.

### Wire format
- `[u32 LE discriminant]` at offset 0.
- Per-variant payload starts at `align_up(4, align_of::<payload>)`.
- Total wire size = `size_of::<Archived>()` (rustc's
  `#[repr(u32)]` layout per RFC 2195). Encoder writes
  per-variant padding + trailing pad to fill the full size;
  `None` writes zero-bytes for the entire payload region.

### Tests
- `tests/kat_tagged.rs` — 16 cases:
  - 5 Option round-trips: `Option<u32>::None`, `Some(u32)`,
    `Some(u8)`, `Some(u64)`, `Option<u64>::None`.
  - 4 Result round-trips: symmetric `(u32, u32)` Ok+Err,
    asymmetric `(u8, u64)` Ok+Err.
  - 6 reject paths: bad disc / truncated / oversize / inner-bool
    reject (Option) + bad disc / truncated (Result).
  - 1 composition smoke: `Option<(u32, u8)>` round-trip.
- `src/tagged.rs::tests` — 3 layout sanity tests
  (`Option<u32>` = 8/4, `Option<u64>` = 16/8, `Result<u8, u64>` = 16/8).

### Gate
- `cargo test -p grpcx-idl`: 81 / 81 passing (was 62 / 62 at
  v0.4 close; +19).
- `cargo test --workspace --skip _under_budget`: 495 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.4.0] - 2026-06-09

**Variable-length types — `Vec<u8>` + `String`.** Fourth L7
checkpoint. First taste of pointer-shaped layout: a `u32 LE`
length prefix + N payload bytes. Scope-cut to byte/UTF-8
payloads — typed `Vec<T>` for `T ≠ u8` defers to v0.5+ (needs
alignment padding when `align_of::<T::Archived>() > 4`). See
[../../REFACTOR-v7-v0.4-varlen.md](../../REFACTOR-v7-v0.4-varlen.md).

### Added
- `src/varlen.rs`:
  - `ArchivedBytes` — `#[repr(transparent)]` DST over `[u8]`.
    Accessors: `len()` / `is_empty()` / `as_slice() -> &[u8]`.
  - `ArchivedString` — `#[repr(transparent)]` over
    `ArchivedBytes`. Accessors: `len()` / `is_empty()` /
    `as_bytes()` / `as_str() -> Result<&str, Error::Utf8>`
    (lazy UTF-8 check — `from_bytes` doesn't run it).
  - `Archive` / `Serialize<W>` / `Deserialize<D>` for
    `Vec<u8>` and `String`.
- `lib.rs` — re-exports `ArchivedBytes` + `ArchivedString`.

### Changed
- `Archive::Archived` is now `?Sized` so DST archived types
  (the varlen flavour) are allowed. Existing tuple + array
  impls add explicit `where T::Archived: Sized` bounds where
  the thin-to-thin cast pattern needs it; the public API
  shape is unchanged for sized archive types.

### Tests
- `tests/kat_varlen.rs` — 14 cases:
  - 5 `Vec<u8>` round-trips: empty, single-byte, short,
    255-byte boundary, 1 KB.
  - 3 `String` round-trips: empty, ASCII, multi-byte UTF-8
    (with emoji).
  - 1 lazy UTF-8: invalid bytes pass `from_bytes` but fail
    `as_str` with `Error::Utf8`.
  - 4 reject paths: truncated header, truncated payload,
    oversize buffer, misaligned buffer.
  - 1 wire-format invariant: u32 LE length prefix byte order.
- `src/varlen.rs::tests` — 1 unit (`#[repr(transparent)]`
  layout sanity).

### Gate
- `cargo test -p grpcx-idl`: 62 / 62 passing (was 47 / 47 at
  v0.3 close; +15).
- `cargo test --workspace --skip _under_budget`: 476 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.3.0] - 2026-06-09

**Fixed-size composite types — tuples 2-8 + fixed arrays.**
Third L7 checkpoint. The wire format follows `#[repr(C)]`
layout: fields at offsets multiples of their own alignment,
explicit zero padding between fields and trailing. See
[../../REFACTOR-v7-v0.3-composites.md](../../REFACTOR-v7-v0.3-composites.md).

### Added
- `src/composite.rs` (new module):
  - `ArchivedTuple2<T0, T1>` … `ArchivedTuple8<T0..T7>` —
    `#[repr(C)]` tuple structs holding each field's `Archived`
    counterpart. Generated via `impl_tuple!` macro.
  - `Archive` / `Serialize<W>` / `Deserialize<D>` impls for
    Rust tuples `(T0, T1)` through `(T0, ..., T7)`. Encoder
    writes explicit zero padding between fields + trailing
    pad to the tuple's own alignment; decoder validates each
    field on its sub-slice + transmutes.
  - `Archive` / `Serialize<W>` / `Deserialize<D>` for
    `[T; N]`. `Archived<[T; N]> = [T::Archived; N]` (Rust
    array layout is naturally contiguous). `Deserialize` uses
    `MaybeUninit` to drop already-initialised slots on error.
  - `align_up(offset, align)` const helper for field-offset
    math.
- `lib.rs` — re-exports `ArchivedTuple2` … `ArchivedTuple8`.

### Tests
- `tests/kat_composites.rs` — 18 cases:
  - 7 tuple round-trips: `(u8, u8)` / `(u16, u32)` /
    `(u32, u8)` (padding) / `(u32, u16, u8)` /
    `(bool, u32)` / `((u32, u8), u16)` nested / 8-tuple max
    arity.
  - 3 tuple reject paths: truncated / oversize / inner-bool
    invalid.
  - 5 array round-trips: `[u8; 4]` / `[u32; 3]` /
    `[(u16, u8); 2]` / `[bool; 8]` / `[u8; 0]` zero-length.
  - 3 array reject paths: truncated / oversize / inner-bool.
- `src/composite.rs::tests` — 2 unit tests
  (`align_up_smoke`, `tuple_size_matches_repr_c`).

### Gate
- `cargo test -p grpcx-idl`: 47 / 47 passing (was 27 / 27 at
  v0.2 close; +20).
- `cargo test --workspace --skip _under_budget`: 461 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.2.0] - 2026-06-09

**Wire format spec + primitive codec.** Second L7 checkpoint.
Locks the little-endian fixed-width wire format and lights up
`Archive` / `Serialize` / `Deserialize` for the 13 numeric
primitives + `bool`. See [../../REFACTOR-v7-v0.2-primitives.md](../../REFACTOR-v7-v0.2-primitives.md).

### Added
- `WIRE_FORMAT.md` at crate root — endianness rule, alignment
  table, padding policy, KAT vector reference.
- `src/primitives.rs` — macro-generated `Archive` /
  `Serialize<W>` / `Deserialize<D>` impls for `u8/u16/u32/u64/
  u128`, `i8/i16/i32/i64/i128`, `f32`, `f64`, plus a hand-
  written `bool` impl (the only primitive with a value-level
  invariant: byte ∈ {0, 1}).
- `Archive::validate(bytes) -> Result<&Archived, Error>` — new
  trait method that the free-function `from_bytes` delegates
  to. Allows per-type structural validation without a separate
  trait.
- LE-only ground rule enforced by a crate-level
  `#[cfg(target_endian = "big")] compile_error!`. Byteswap
  accessor wrappers defer to v1.x.

### Filled bodies
- `to_bytes` and `from_bytes` are no longer
  `unimplemented!()` — the free functions drive
  `Serialize::serialize` into a fresh `Vec<u8>` and
  `Archive::validate` respectively.

### Tests
- `src/primitives.rs::tests` (8 cases): u8 / u32 LE shape /
  i16 negative / f32 unity / bool both values / bool reject /
  truncated reject / oversize reject.
- `tests/kat_primitives.rs` (17 cases): one KAT per primitive
  + truncated + oversize + non-canonical bool + an every-
  primitive round-trip smoke that locks the macro shape.

### Gate
- `cargo test -p grpcx-idl`: 27 / 27 (was 2; +25).
- `cargo test --workspace --skip _under_budget`: 441 / 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning.
- `cargo fmt --check`: clean.

## [0.1.0] - 2026-06-09

**API skeleton + design docs.** First L7 checkpoint. The public
surface is frozen and each item carries a doc comment, but
bodies are `unimplemented!()`. Implementation lands across
v0.2-v0.7; see [../../REFACTOR-v7-v0.1-bootstrap.md](../../REFACTOR-v7-v0.1-bootstrap.md).

### Added
- `Archive` — trait marking a Rust type as having a defined
  zero-copy in-place layout (`Archived<T>`). The associated
  type carries the `Archived` byte view that `from_bytes`
  returns by reference.
- `Serialize<W>` — trait for emitting a value into a writer.
  The writer is a small trait so the crate stays
  `no_std + alloc`-friendly.
- `Deserialize<D>` — trait for materialising an owned `T` from
  an archived view + a deserialization context (the context
  carries any extra borrow lifetimes / allocators a real
  implementation may need).
- `Writer` — minimal byte-sink trait (push bytes; track
  current offset). Implemented for `Vec<u8>` out of the box.
- `to_bytes(value) -> Result<Vec<u8>, Error>` — drive
  serialization end-to-end with a default writer.
- `from_bytes::<T>(bytes) -> Result<&Archived<T>, Error>` —
  validate + cast bytes to the zero-copy archived view.
- `Error` — failure modes (`Truncated` / `BadLayout` /
  `BadAlignment` / `Utf8` / `Custom(&'static str)`).
- `Cargo.toml` — full published metadata; `version = 0.1.0`
  (literal); no runtime deps (L7 is a pure-codec layer).
- `README.md`, `BUDGETS.md`, this `CHANGELOG.md`.
- Workspace `Cargo.toml` adds `crates/grpcx-idl` as the
  seventh member; `[exclude]` reserves
  `crates/grpcx-wt/fuzz` (v0.7 carry-over).

### Gate
- `cargo test --workspace`: workspace builds clean, no L7 tests
  yet (skeleton bodies are `unimplemented!()`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo doc --workspace --no-deps`: zero-warning across the
  `missing_docs` / `broken_intra_doc_links` denies.
- `cargo fmt --check`: clean.
