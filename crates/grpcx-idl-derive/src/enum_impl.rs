//! Codegen for `#[derive(GrpcxIdl)] enum E { … }`.
//!
//! Supports two variant shapes per v0.6 MVP:
//! - Unit: `E::Idle`.
//! - Single-payload tuple: `E::Running(u32)`.
//!
//! Multi-payload tuple variants + named-field variants reject
//! with a compile error pointing the user at v0.6.1+.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DataEnum, DeriveInput, Fields, GenericParam, Generics};

use crate::{align_up_snippet, archived_ident};

pub(crate) fn derive_enum(input: &DeriveInput, data: &DataEnum) -> TokenStream2 {
    let vis = &input.vis;
    let source_ident = &input.ident;
    let archived_ident = archived_ident(source_ident);
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Classify each variant; reject unsupported shapes early.
    enum VariantShape<'a> {
        Unit,
        SinglePayload(&'a syn::Type),
    }

    let mut variants_info: Vec<(syn::Ident, VariantShape, u32)> = Vec::new();
    for (disc, v) in data.variants.iter().enumerate() {
        let disc_u32 = disc as u32;
        match &v.fields {
            Fields::Unit => {
                variants_info.push((v.ident.clone(), VariantShape::Unit, disc_u32));
            }
            Fields::Unnamed(unn) => {
                if unn.unnamed.len() != 1 {
                    return syn::Error::new_spanned(
                        v,
                        "#[derive(GrpcxIdl)] in v0.6 supports only \
                         single-payload tuple variants (e.g. `Some(T)`); \
                         multi-payload `Variant(A, B)` lands in v0.6.1.",
                    )
                    .to_compile_error();
                }
                let ty = &unn.unnamed.first().unwrap().ty;
                variants_info.push((v.ident.clone(), VariantShape::SinglePayload(ty), disc_u32));
            }
            Fields::Named(_) => {
                return syn::Error::new_spanned(
                    v,
                    "#[derive(GrpcxIdl)] in v0.6 does not support struct \
                     variants (`E::X { a: A }`); use a tuple variant with a \
                     named-field struct payload, or wait for v0.6.1.",
                )
                .to_compile_error();
            }
        }
    }

    // Collect each variant's payload type (for bound generation).
    let payload_types: Vec<&syn::Type> = variants_info
        .iter()
        .filter_map(|(_, shape, _)| match shape {
            VariantShape::Unit => None,
            VariantShape::SinglePayload(ty) => Some(*ty),
        })
        .collect();
    let archive_bounds = generate_archive_bounds(&payload_types);

    let user_where_predicates = match &input.generics.where_clause {
        Some(wc) => {
            let preds = &wc.predicates;
            quote! { #preds, }
        }
        None => quote! {},
    };

    let align_up = align_up_snippet();

    // Build the archived enum's variants.
    let archived_variants = variants_info.iter().map(|(name, shape, disc)| match shape {
        VariantShape::Unit => quote! { #name = #disc },
        VariantShape::SinglePayload(ty) => {
            quote! { #name(<#ty as ::grpcx_idl::Archive>::Archived) = #disc }
        }
    });

    // Validate arms (per disc value).
    let validate_arms = variants_info.iter().map(|(_, shape, disc)| match shape {
        VariantShape::Unit => quote! { #disc => {} },
        VariantShape::SinglePayload(ty) => {
            quote! {
                #disc => {
                    let payload_off = __grpcx_idl_align_up(
                        4,
                        ::core::mem::align_of::<<#ty as ::grpcx_idl::Archive>::Archived>(),
                    );
                    let payload_size =
                        ::core::mem::size_of::<<#ty as ::grpcx_idl::Archive>::Archived>();
                    <#ty as ::grpcx_idl::Archive>::validate(
                        &bytes[payload_off..payload_off + payload_size],
                    )?;
                }
            }
        }
    });

    // Serialize arms — match on source enum value.
    let serialize_arms = variants_info.iter().map(|(name, shape, disc)| match shape {
        VariantShape::Unit => quote! {
            #source_ident::#name => {
                ::grpcx_idl::Writer::write_all(writer, &(#disc as u32).to_le_bytes())?;
                let total = ::core::mem::size_of::<<Self as ::grpcx_idl::Archive>::Archived>();
                let mut _cursor: usize = 4;
                while _cursor < total {
                    ::grpcx_idl::Writer::write_all(writer, &[0u8])?;
                    _cursor += 1;
                }
            }
        },
        VariantShape::SinglePayload(ty) => quote! {
            #source_ident::#name(__inner) => {
                ::grpcx_idl::Writer::write_all(writer, &(#disc as u32).to_le_bytes())?;
                let payload_off = __grpcx_idl_align_up(
                    4,
                    ::core::mem::align_of::<<#ty as ::grpcx_idl::Archive>::Archived>(),
                );
                let mut _cursor: usize = 4;
                while _cursor < payload_off {
                    ::grpcx_idl::Writer::write_all(writer, &[0u8])?;
                    _cursor += 1;
                }
                ::grpcx_idl::Serialize::serialize(__inner, writer)?;
                _cursor = payload_off
                    + ::core::mem::size_of::<<#ty as ::grpcx_idl::Archive>::Archived>();
                let total = ::core::mem::size_of::<<Self as ::grpcx_idl::Archive>::Archived>();
                while _cursor < total {
                    ::grpcx_idl::Writer::write_all(writer, &[0u8])?;
                    _cursor += 1;
                }
            }
        },
    });

    // Deserialize arms — match on archived enum value.
    let deserialize_arms = variants_info.iter().map(|(name, shape, _)| match shape {
        VariantShape::Unit => quote! {
            #archived_ident::#name => Ok(#source_ident::#name)
        },
        VariantShape::SinglePayload(ty) => {
            quote! {
                #archived_ident::#name(__inner) => Ok(#source_ident::#name(
                    <#ty as ::grpcx_idl::Deserialize<__GrpcxIdlD>>::deserialize(__inner, ctx)?,
                ))
            }
        }
    });

    let augmented_serialize = augmented_impl_generics(&input.generics, "__GrpcxIdlW");
    let augmented_deserialize = augmented_impl_generics(&input.generics, "__GrpcxIdlD");

    let serialize_field_bounds = payload_types.iter().map(|ty| {
        quote! { #ty: ::grpcx_idl::Serialize<__GrpcxIdlW>, }
    });
    let deserialize_field_bounds = payload_types.iter().map(|ty| {
        quote! { #ty: ::grpcx_idl::Deserialize<__GrpcxIdlD>, }
    });

    quote! {
        #[repr(u32)]
        #[allow(non_snake_case)]
        #vis enum #archived_ident #ty_generics
        where
            #user_where_predicates
            #archive_bounds
        {
            #(#archived_variants),*
        }

        impl #impl_generics ::grpcx_idl::Archive for #source_ident #ty_generics
        #where_clause
        where
            #user_where_predicates
            #archive_bounds
        {
            type Archived = #archived_ident #ty_generics;

            fn validate(bytes: &[u8]) -> ::core::result::Result<&Self::Archived, ::grpcx_idl::Error> {
                #align_up

                let need = ::core::mem::size_of::<Self::Archived>();
                if bytes.len() < need {
                    return Err(::grpcx_idl::Error::Truncated);
                }
                if bytes.len() != need {
                    return Err(::grpcx_idl::Error::BadLayout);
                }
                let align = ::core::mem::align_of::<Self::Archived>();
                if (bytes.as_ptr() as usize) % align != 0 {
                    return Err(::grpcx_idl::Error::BadAlignment);
                }
                let disc =
                    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                match disc {
                    #(#validate_arms)*
                    _ => return Err(::grpcx_idl::Error::BadLayout),
                }
                // SAFETY: see struct_impl — same shape, per-arm validation.
                Ok(unsafe { &*(bytes.as_ptr() as *const Self::Archived) })
            }
        }

        impl #augmented_serialize ::grpcx_idl::Serialize<__GrpcxIdlW> for #source_ident #ty_generics
        where
            __GrpcxIdlW: ::grpcx_idl::Writer,
            #user_where_predicates
            #archive_bounds
            #(#serialize_field_bounds)*
        {
            fn serialize(
                &self,
                writer: &mut __GrpcxIdlW,
            ) -> ::core::result::Result<(), ::grpcx_idl::Error> {
                #align_up
                match self {
                    #(#serialize_arms)*
                }
                Ok(())
            }
        }

        impl #augmented_deserialize ::grpcx_idl::Deserialize<__GrpcxIdlD> for #source_ident #ty_generics
        where
            #user_where_predicates
            #archive_bounds
            #(#deserialize_field_bounds)*
        {
            fn deserialize(
                archived: &Self::Archived,
                ctx: &mut __GrpcxIdlD,
            ) -> ::core::result::Result<Self, ::grpcx_idl::Error> {
                match archived {
                    #(#deserialize_arms,)*
                }
            }
        }
    }
}

fn augmented_impl_generics(generics: &Generics, fresh_name: &str) -> TokenStream2 {
    let fresh = syn::Ident::new(fresh_name, proc_macro2::Span::call_site());
    let user_params = generics.params.iter().map(|p| match p {
        GenericParam::Type(t) => {
            let ident = &t.ident;
            let bounds = &t.bounds;
            if bounds.is_empty() {
                quote! { #ident }
            } else {
                quote! { #ident: #bounds }
            }
        }
        GenericParam::Const(c) => {
            let ident = &c.ident;
            let ty = &c.ty;
            quote! { const #ident: #ty }
        }
        GenericParam::Lifetime(_) => quote! {},
    });
    quote! { <#fresh, #(#user_params),*> }
}

fn generate_archive_bounds(payload_types: &[&syn::Type]) -> TokenStream2 {
    let pairs = payload_types.iter().map(|ty| {
        quote! {
            #ty: ::grpcx_idl::Archive,
            <#ty as ::grpcx_idl::Archive>::Archived: ::core::marker::Sized,
        }
    });
    quote! { #(#pairs)* }
}
