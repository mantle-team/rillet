//! Implementation of the `#[derive(Event)]` macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Result};

/// Expands `#[derive(Event)]`.
pub fn expand(input: DeriveInput) -> Result<TokenStream> {
    let name = &input.ident;

    Ok(quote! {
        impl rillet::event::Event for #name {}
    })
}
