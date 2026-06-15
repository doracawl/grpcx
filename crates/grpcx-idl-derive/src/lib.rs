//! `#[derive(GrpcxIdl)]` — auto-generate `Archive` /
//! `Serialize` / `Deserialize` impls for user structs +
//! enums, matching the L7 wire format hand-rolled in v0.2 →
//! v0.5.
//!
//! Supported shapes (v0.6 MVP):
//! - Named-field structs: `struct Foo { a: A, b: B }`.
//! - Enums with unit variants and single-payload tuple
//!   variants: `enum E { Idle, Running(u32), Done }`.
//!
//! Out of scope for v0.6 (clear compile-error if used):
//! - Tuple structs (`struct Foo(A, B);`).
//! - Multi-field tuple variants (`E::X(A, B)`).
//! - Named-field struct variants (`E::X { a: A }`).
//! - Lifetime generic params (`struct Foo<'a> { ... }`).
//!
//! These all defer to v0.6.1 / v1.x.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, parse_macro_input};

mod enum_impl;
mod struct_impl;

/// `#[derive(GrpcxIdl)]` — see crate-level docs.
#[proc_macro_derive(GrpcxIdl)]
pub fn derive_grpcx_idl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    // Reject lifetime generics at the macro level — `Archive::Archived`
    // can't carry a lifetime.
    for param in &input.generics.params {
        if matches!(param, syn::GenericParam::Lifetime(_)) {
            return syn::Error::new_spanned(
                param,
                "#[derive(GrpcxIdl)] does not support lifetime parameters \
                 (Archive::Archived can't carry a lifetime; \
                 use owned types like Vec<u8> / String for variable-length \
                 fields, which give zero-copy access through their \
                 archived counterparts).",
            )
            .to_compile_error()
            .into();
        }
    }

    let expanded: TokenStream2 = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => struct_impl::derive_named_struct(&input, &named.named),
            Fields::Unnamed(_) => syn::Error::new_spanned(
                &input,
                "#[derive(GrpcxIdl)] for tuple structs is not supported in \
                 v0.6 — wrap the fields in a named-field struct or use \
                 `(A, B, ...)` directly (Rust tuples already have an \
                 Archive impl).",
            )
            .to_compile_error(),
            Fields::Unit => syn::Error::new_spanned(
                &input,
                "#[derive(GrpcxIdl)] for unit structs is not supported in \
                 v0.6 — unit types have size 0, which the L7 wire format \
                 currently doesn't accept as a top-level value.",
            )
            .to_compile_error(),
        },
        Data::Enum(data) => enum_impl::derive_enum(&input, data),
        Data::Union(_) => syn::Error::new_spanned(
            &input,
            "#[derive(GrpcxIdl)] does not support Rust unions — use a \
             tagged enum instead.",
        )
        .to_compile_error(),
    };

    expanded.into()
}

// ============================================================
// Shared utilities used by both struct_impl and enum_impl.
// ============================================================

/// `ArchivedFoo` from `Foo`.
pub(crate) fn archived_ident(source: &syn::Ident) -> syn::Ident {
    format_ident!("Archived{}", source)
}

/// `align_up` math snippet — inlined into each generated impl
/// so the consumer crate doesn't need to access `grpcx_idl`'s
/// `pub(crate) fn align_up`.
pub(crate) fn align_up_snippet() -> TokenStream2 {
    quote! {
        const fn __grpcx_idl_align_up(offset: usize, align: usize) -> usize {
            (offset + align - 1) & !(align - 1)
        }
    }
}
