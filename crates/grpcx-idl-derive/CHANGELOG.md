# Changelog

All notable changes to `grpcx-idl-derive` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

(see [1.0.0] below)

## [1.0.0] - 2026-06-09

**Project v7.0 release** — `grpcx-idl-derive` ships at v1.0
alongside `grpcx-idl` 1.0.0. The proc-macro version stays in
lock-step with the parent crate's L7 release cadence. See
[../../REFACTOR-v7.0-release.md](../../REFACTOR-v7.0-release.md).

### Added (originally in v0.1.0, 2026-06-09)
- `#[derive(GrpcxIdl)]` proc-macro:
  - `src/lib.rs` — dispatches by `syn::Data` shape; rejects
    lifetime generics / unions / unit structs / tuple structs
    / multi-payload tuple variants / named-field variants with
    descriptive compile errors.
  - `src/struct_impl.rs` — codegen for named-field structs.
    Emits `#[repr(C)] struct ArchivedFoo` + `Archive` +
    `Serialize<W>` + `Deserialize<D>` impls.
  - `src/enum_impl.rs` — codegen for enums with unit + single-
    payload tuple variants. Emits `#[repr(u32)] enum
    ArchivedFoo` + the three impls.

### v1.0 status
- Tested via `grpcx-idl`'s derive KAT (14 cases, all green).
- API stable; future macro shapes (tuple structs, multi-
  payload variants, `#[grpcx_idl(skip)]`) ship as additive
  features in v1.x.
