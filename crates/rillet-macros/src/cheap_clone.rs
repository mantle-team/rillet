//! Implementation of the `#[derive(CheapClone)]` macro.
//!
//! The derive emits an impl with a `CheapClone` bound per field, each
//! spanned to its field, so a field whose type lacks the marker fails to
//! compile at that field.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Error, Result, Type};

/// Expands `#[derive(CheapClone)]`.
pub fn expand(input: DeriveInput) -> Result<TokenStream> {
    let name = &input.ident;

    let field_types: Vec<&Type> = match &input.data {
        Data::Struct(data) => data.fields.iter().map(|f| &f.ty).collect(),
        Data::Enum(data) => data
            .variants
            .iter()
            .flat_map(|v| v.fields.iter().map(|f| &f.ty))
            .collect(),
        Data::Union(_) => {
            return Err(Error::new_spanned(
                &input,
                "CheapClone cannot be derived for unions",
            ));
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let field_bounds: Vec<TokenStream> = field_types
        .iter()
        .map(|ty| quote_spanned! {ty.span()=> #ty: rillet::view::CheapClone })
        .collect();

    let where_clause = match where_clause {
        Some(existing) => {
            let predicates = &existing.predicates;
            quote! { where #predicates, #(#field_bounds),* }
        }
        None if field_bounds.is_empty() => quote! {},
        None => quote! { where #(#field_bounds),* },
    };

    Ok(quote! {
        impl #impl_generics rillet::view::CheapClone for #name #ty_generics #where_clause {}
    })
}
