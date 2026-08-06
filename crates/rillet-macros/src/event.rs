//! Implementation of the `#[derive(Event)]` macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Error, Result};

/// Expands `#[derive(Event)]`.
pub fn expand(input: DeriveInput) -> Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &input.generics,
            "Event cannot be derived for generic types",
        ));
    }

    let name = &input.ident;

    Ok(quote! {
        impl rillet::event::Event for #name {}
    })
}
