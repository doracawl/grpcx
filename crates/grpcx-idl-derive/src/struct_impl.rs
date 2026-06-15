//! Codegen for `#[derive(GrpcxIdl)] struct Foo { … }`.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DeriveInput, Field, GenericParam, Generics, punctuated::Punctuated, token::Comma};

use crate::{align_up_snippet, archived_ident};

pub(crate) fn derive_named_struct(
    input: &DeriveInput,
    fields: &Punctuated<Field, Comma>,
) -> TokenStream2 {
    let vis = &input.vis;
    let source_ident = &input.ident;
    let archived_ident = archived_ident(source_ident);
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Collect per-field info.
    let field_names: Vec<_> = fields
        .iter()
        .map(|f| f.ident.as_ref().expect("named struct fields have idents"))
        .collect();
    let field_types: Vec<_> = fields.iter().map(|f| &f.ty).collect();
    let field_vis: Vec<_> = fields.iter().map(|f| &f.vis).collect();

    let archive_bounds = generate_archive_bounds(&field_types);
    let user_where_predicates = match &input.generics.where_clause {
        Some(wc) => {
            let preds = &wc.predicates;
            quote! { #preds, }
        }
        None => quote! {},
    };

    let align_up = align_up_snippet();

    // Validate walk: field-by-field, computing each offset.
    let validate_walk = field_types.iter().map(|ty| {
        quote! {
            _cursor = __grpcx_idl_align_up(
                _cursor,
                ::core::mem::align_of::<<#ty as ::grpcx_idl::Archive>::Archived>(),
            );
            let _size = ::core::mem::size_of::<<#ty as ::grpcx_idl::Archive>::Archived>();
            <#ty as ::grpcx_idl::Archive>::validate(&bytes[_cursor.._cursor + _size])?;
            _cursor += _size;
        }
    });

    // Serialize walk.
    let serialize_walk = field_names
        .iter()
        .zip(field_types.iter())
        .map(|(name, ty)| {
            quote! {
                let want = __grpcx_idl_align_up(
                    _cursor,
                    ::core::mem::align_of::<<#ty as ::grpcx_idl::Archive>::Archived>(),
                );
                while _cursor < want {
                    ::grpcx_idl::Writer::write_all(writer, &[0u8])?;
                    _cursor += 1;
                }
                ::grpcx_idl::Serialize::serialize(&self.#name, writer)?;
                _cursor += ::core::mem::size_of::<<#ty as ::grpcx_idl::Archive>::Archived>();
            }
        });

    // Deserialize: rebuild each field.
    let deserialize_fields = field_names.iter().zip(field_types.iter()).map(|(name, ty)| {
        quote! {
            #name: <#ty as ::grpcx_idl::Deserialize<__GrpcxIdlD>>::deserialize(&archived.#name, ctx)?
        }
    });

    // For Serialize / Deserialize we need a fresh generic param
    // (W or D) on top of the user's existing generics. Build the
    // augmented `<W, T0, T1, ...>` impl generics manually.
    let augmented_serialize = augmented_impl_generics(&input.generics, "__GrpcxIdlW");
    let augmented_deserialize = augmented_impl_generics(&input.generics, "__GrpcxIdlD");

    // Serialize bounds need each field type to also impl Serialize<W>.
    let serialize_field_bounds = field_types.iter().map(|ty| {
        quote! { #ty: ::grpcx_idl::Serialize<__GrpcxIdlW>, }
    });
    let deserialize_field_bounds = field_types.iter().map(|ty| {
        quote! { #ty: ::grpcx_idl::Deserialize<__GrpcxIdlD>, }
    });

    quote! {
        #[repr(C)]
        #[allow(non_snake_case)]
        #vis struct #archived_ident #ty_generics
        where
            #user_where_predicates
            #archive_bounds
        {
            #(
                #field_vis #field_names: <#field_types as ::grpcx_idl::Archive>::Archived,
            )*
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

                let mut _cursor: usize = 0;
                #(#validate_walk)*

                // SAFETY:
                // - `#archived_ident` is `#[repr(C)]`, so its
                //   in-memory layout matches the SysV-ABI wire
                //   layout walked above.
                // - length + alignment + per-field invariants
                //   all checked.
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

                let mut _cursor: usize = 0;
                #(#serialize_walk)*
                let total = ::core::mem::size_of::<<Self as ::grpcx_idl::Archive>::Archived>();
                while _cursor < total {
                    ::grpcx_idl::Writer::write_all(writer, &[0u8])?;
                    _cursor += 1;
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
                Ok(Self {
                    #(#deserialize_fields,)*
                })
            }
        }
    }
}

/// Build `<__GrpcxIdlW, T0, T1>` from the user's generics
/// `<T0, T1>` plus a fresh type param ident.
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
        GenericParam::Lifetime(_) => {
            // Rejected upstream — lifetimes don't fit.
            quote! {}
        }
    });
    quote! { <#fresh, #(#user_params),*> }
}

/// Generate `T0: ::grpcx_idl::Archive, <T0 as ::grpcx_idl::Archive>::Archived: Sized,` …
fn generate_archive_bounds(field_types: &[&syn::Type]) -> TokenStream2 {
    let pairs = field_types.iter().map(|ty| {
        quote! {
            #ty: ::grpcx_idl::Archive,
            <#ty as ::grpcx_idl::Archive>::Archived: ::core::marker::Sized,
        }
    });
    quote! { #(#pairs)* }
}
