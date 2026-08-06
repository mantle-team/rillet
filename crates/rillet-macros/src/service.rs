//! Implementation of the `#[rillet::service]` attribute macro.
//!
//! The macro:
//! - Injects an emitter field for services that emit events
//! - Injects a view slot for services that publish a view
//! - Generates getters, subscription methods, and emit_<event>() methods
//! - Generates the view handle type for view-publishing services

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Error, Expr, Field, Fields, FieldsNamed, Ident, Result, Type, parse::Parser,
    parse2,
};

/// Expands `#[rillet::service]`.
pub fn expand(attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let args = parse_service_args(attr)?;
    let view_type = args.view_type;
    let mut input: DeriveInput = parse2(item)?;

    let emitted_events = parse_struct_attrs(&input)?;

    let fields = match &mut input.data {
        Data::Struct(data) => match &mut data.fields {
            Fields::Named(fields) => fields,
            _ => {
                return Err(Error::new_spanned(
                    &input,
                    "rillet::service can only be applied to structs with named fields",
                ));
            }
        },
        _ => {
            return Err(Error::new_spanned(
                &input,
                "rillet::service can only be applied to structs",
            ));
        }
    };

    let struct_name = &input.ident;
    let handle_name = format_ident!("{}Handle", struct_name);

    // Getters, defaults, and constructor fields come from the user's
    // declaration, so they are collected before hidden fields are injected.
    let getters: Vec<GetterField> = fields
        .named
        .iter()
        .filter_map(|f| GetterField::from_field(f).transpose())
        .collect::<Result<_>>()?;

    let defaults: Vec<DefaultField> = fields
        .named
        .iter()
        .filter_map(|f| DefaultField::from_field(f).transpose())
        .collect::<Result<_>>()?;

    let user_fields: Vec<_> = fields
        .named
        .iter()
        .map(|f| (f.ident.clone().unwrap(), f.ty.clone()))
        .collect();

    let has_emitter = !emitted_events.is_empty();
    if has_emitter {
        inject_emitter_field(fields);
    }

    if let Some(view_ty) = &view_type {
        inject_view_slot_field(fields, view_ty);
    }

    inject_cancel_token_field(fields);
    inject_join_handles_field(fields);
    inject_sampling_state_field(fields);

    // The handle struct itself is generated in handlers.rs, which knows the
    // command count the metrics field needs.
    let getter_impls = generate_getters(&handle_name, &getters);
    let subscription_methods = generate_subscription_methods(&handle_name, &emitted_events);
    let emit_methods = generate_emit_methods(struct_name, &emitted_events, args.event_capacity);
    let constructor = generate_constructor(
        struct_name,
        &user_fields,
        &defaults,
        has_emitter,
        view_type.is_some(),
    );
    let shutdown_method = generate_shutdown_method(&handle_name);
    let view_impls = generate_view_impls(
        struct_name,
        &handle_name,
        view_type.as_ref(),
        &emitted_events,
    );

    let command_capacity = args.command_capacity;
    let capacity_const = quote! {
        impl #struct_name {
            #[doc(hidden)]
            pub const __RILLET_COMMAND_CAPACITY: usize = #command_capacity;
        }
    };

    // Strip the processed rillet attributes from the struct and its fields.
    input.attrs.retain(|attr| !attr.path().is_ident("rillet"));

    if let Data::Struct(data) = &mut input.data
        && let Fields::Named(fields) = &mut data.fields
    {
        for field in &mut fields.named {
            field.attrs.retain(|attr| !attr.path().is_ident("rillet"));
        }
    }

    Ok(quote! {
        #input
        #getter_impls
        #subscription_methods
        #emit_methods
        #constructor
        #shutdown_method
        #view_impls
        #capacity_const
    })
}

/// The parsed arguments of the service attribute itself.
struct ServiceArgs {
    view_type: Option<Type>,
    command_capacity: usize,
    event_capacity: usize,
}

/// Parse `view = MyView, command_capacity = N, event_capacity = N` from the
/// attribute arguments.
fn parse_service_args(attr: TokenStream) -> Result<ServiceArgs> {
    let mut args = ServiceArgs {
        view_type: None,
        command_capacity: 256,
        event_capacity: 256,
    };
    if attr.is_empty() {
        return Ok(args);
    }

    let metas =
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated.parse2(attr)?;

    for meta in metas {
        match &meta {
            syn::Meta::NameValue(nv) if nv.path.is_ident("view") => {
                let value = &nv.value;
                args.view_type = Some(parse2(quote! { #value })?);
            }
            syn::Meta::NameValue(nv) if nv.path.is_ident("command_capacity") => {
                args.command_capacity = parse_capacity(&nv.value)?;
            }
            syn::Meta::NameValue(nv) if nv.path.is_ident("event_capacity") => {
                args.event_capacity = parse_capacity(&nv.value)?;
            }
            _ => {
                return Err(Error::new_spanned(
                    meta,
                    "expected `view = ViewType`, `command_capacity = N`, or `event_capacity = N`",
                ));
            }
        }
    }

    Ok(args)
}

fn parse_capacity(value: &Expr) -> Result<usize> {
    if let Expr::Lit(lit) = value
        && let syn::Lit::Int(int) = &lit.lit
    {
        let capacity: usize = int.base10_parse()?;
        if capacity == 0 {
            return Err(Error::new_spanned(value, "capacity must be at least 1"));
        }
        return Ok(capacity);
    }
    Err(Error::new_spanned(value, "expected an integer literal"))
}

/// Parse the emitted events from the struct's #[rillet(...)] attributes.
fn parse_struct_attrs(input: &DeriveInput) -> Result<Vec<Ident>> {
    let mut emitted_events = Vec::new();

    for attr in &input.attrs {
        if !attr.path().is_ident("rillet") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("emits") {
                meta.input.parse::<syn::Token![=]>()?;

                // Parse [Event1, Event2, ...]
                let content;
                syn::bracketed!(content in meta.input);

                while !content.is_empty() {
                    let event: Ident = content.parse()?;
                    emitted_events.push(event);

                    if content.is_empty() {
                        break;
                    }
                    content.parse::<syn::Token![,]>()?;
                }
            }
            Ok(())
        })?;
    }

    Ok(emitted_events)
}

/// Inject a hidden emitter field into the struct.
fn inject_emitter_field(fields: &mut FieldsNamed) {
    let emitter_field: syn::Field = syn::parse_quote! {
        #[doc(hidden)]
        __rillet_emitter: rillet::event::Emitter
    };
    fields.named.push(emitter_field);
}

/// Inject a hidden view slot field into the struct.
///
/// The slot is seeded at spawn, once the initial state exists to compute a
/// view from.
fn inject_view_slot_field(fields: &mut FieldsNamed, view_ty: &Type) {
    let view_field: syn::Field = syn::parse_quote! {
        #[doc(hidden)]
        __rillet_view_slot: Option<rillet::view::ViewSlot<#view_ty>>
    };
    fields.named.push(view_field);
}

/// Inject a hidden cancellation token field into the struct.
fn inject_cancel_token_field(fields: &mut FieldsNamed) {
    let cancel_field: syn::Field = syn::parse_quote! {
        #[doc(hidden)]
        __rillet_cancel_token: rillet::runtime::CancellationToken
    };
    fields.named.push(cancel_field);
}

/// Inject a hidden join handles field into the struct for task completion tracking.
fn inject_join_handles_field(fields: &mut FieldsNamed) {
    let join_handles_field: syn::Field = syn::parse_quote! {
        #[doc(hidden)]
        __rillet_join_handles: rillet::runtime::Arc<rillet::runtime::Mutex<Vec<Box<dyn rillet::runtime::TaskHandle>>>>
    };
    fields.named.push(join_handles_field);
}

/// Inject a hidden sampling state field into the struct for metrics.
fn inject_sampling_state_field(fields: &mut FieldsNamed) {
    let sampling_state_field: syn::Field = syn::parse_quote! {
        #[doc(hidden)]
        __rillet_sampling_state: rillet::metrics::SamplingState
    };
    fields.named.push(sampling_state_field);
}

// ============================================================================
// Field Parsing
// ============================================================================

struct GetterField {
    name: Ident,
    ty: Type,
}

impl GetterField {
    fn from_field(field: &Field) -> Result<Option<Self>> {
        let name = field
            .ident
            .clone()
            .expect("we already verified these are named fields");

        // Skip injected rillet fields
        if name.to_string().starts_with("__rillet_") {
            return Ok(None);
        }

        for attr in &field.attrs {
            if !attr.path().is_ident("rillet") {
                continue;
            }

            let mut is_getter = false;

            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("get") {
                    is_getter = true;
                } else if meta.input.peek(syn::Token![=]) {
                    // Skip over unknown metas with = value
                    let _: syn::Expr = meta.value()?.parse()?;
                } else if meta.input.peek(syn::token::Paren) {
                    // Skip over unknown metas with (...)
                    let _content;
                    syn::parenthesized!(_content in meta.input);
                }
                // A bare path like #[rillet(something)] needs no skipping.
                Ok(())
            })?;

            if is_getter {
                return Ok(Some(Self {
                    name,
                    ty: field.ty.clone(),
                }));
            }
        }

        Ok(None)
    }
}

struct DefaultField {
    name: Ident,
    #[allow(dead_code)]
    ty: Type,
    default_expr: Option<Expr>,
}

impl DefaultField {
    fn from_field(field: &Field) -> Result<Option<Self>> {
        let name = field
            .ident
            .clone()
            .expect("we already verified these are named fields");

        // Skip injected rillet fields
        if name.to_string().starts_with("__rillet_") {
            return Ok(None);
        }

        for attr in &field.attrs {
            if !attr.path().is_ident("rillet") {
                continue;
            }

            let mut default_expr: Option<Option<Expr>> = None;

            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    if meta.input.peek(syn::Token![=]) {
                        let value = meta.value()?;
                        let expr: Expr = value.parse()?;
                        default_expr = Some(Some(expr));
                    } else {
                        // Bare #[rillet(default)] uses Default::default().
                        default_expr = Some(None);
                    }
                }
                Ok(())
            })?;

            if let Some(expr) = default_expr {
                return Ok(Some(Self {
                    name,
                    ty: field.ty.clone(),
                    default_expr: expr,
                }));
            }
        }

        Ok(None)
    }
}

// ============================================================================
// Code Generation
// ============================================================================

fn generate_getters(handle_name: &Ident, getters: &[GetterField]) -> TokenStream {
    let getter_methods: Vec<TokenStream> = getters
        .iter()
        .map(|getter| {
            let field_name = &getter.name;
            let field_type = &getter.ty;

            quote! {
                pub fn #field_name(&self) -> #field_type {
                    self.state.read().unwrap().#field_name.clone()
                }
            }
        })
        .collect();

    if getter_methods.is_empty() {
        return quote! {};
    }

    quote! {
        impl #handle_name {
            #(#getter_methods)*
        }
    }
}

fn generate_subscription_methods(handle_name: &Ident, emitted_events: &[Ident]) -> TokenStream {
    if emitted_events.is_empty() {
        return quote! {};
    }

    let subscription_methods: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            let method_name = format_ident!("on_{}", to_snake_case(&event.to_string()));

            quote! {
                /// Returns a receiver for all future events of this type.
                pub fn #method_name(&self) -> rillet::event::EventReceiver<#event> {
                    rillet::event::EventReceiver::new(
                        self.state.read().unwrap().__rillet_emitter.subscribe::<#event>()
                    )
                }
            }
        })
        .collect();

    let emit_methods: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            let method_name = format_ident!("emit_{}", to_snake_case(&event.to_string()));

            quote! {
                /// Emits the event to all subscribers.
                pub fn #method_name(&self, event: #event) {
                    self.state.read().unwrap().__rillet_emitter.emit(event);
                }
            }
        })
        .collect();

    let stats_methods: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            let published_method = format_ident!("{}_published", to_snake_case(&event.to_string()));
            let subscribers_method =
                format_ident!("{}_subscriber_count", to_snake_case(&event.to_string()));

            quote! {
                /// Returns the total number of events published for this event type.
                pub fn #published_method(&self) -> u64 {
                    self.state.read().unwrap().__rillet_emitter.published::<#event>()
                }

                /// Returns the current number of subscribers for this event type.
                pub fn #subscribers_method(&self) -> usize {
                    self.state.read().unwrap().__rillet_emitter.subscriber_count::<#event>()
                }
            }
        })
        .collect();

    quote! {
        impl #handle_name {
            #(#subscription_methods)*
            #(#emit_methods)*
            #(#stats_methods)*
        }
    }
}

/// Generate the `new()` constructor that initializes all fields.
fn generate_constructor(
    struct_name: &Ident,
    user_fields: &[(Ident, Type)],
    defaults: &[DefaultField],
    has_emitter: bool,
    has_view: bool,
) -> TokenStream {
    let default_names: std::collections::HashSet<_> = defaults.iter().map(|d| &d.name).collect();

    // Only non-defaulted fields become parameters.
    let params: Vec<TokenStream> = user_fields
        .iter()
        .filter(|(name, _)| !default_names.contains(name))
        .map(|(name, ty)| quote! { #name: #ty })
        .collect();

    // Every field needs an initializer.
    let field_inits: Vec<TokenStream> = user_fields
        .iter()
        .map(|(name, _)| {
            if let Some(default) = defaults.iter().find(|d| &d.name == name) {
                match &default.default_expr {
                    Some(expr) => quote! { #name: #expr },
                    None => quote! { #name: Default::default() },
                }
            } else {
                quote! { #name }
            }
        })
        .collect();

    let emitter_init = if has_emitter {
        quote! { __rillet_emitter: rillet::event::Emitter::new(), }
    } else {
        quote! {}
    };

    let view_init = if has_view {
        quote! { __rillet_view_slot: None, }
    } else {
        quote! {}
    };

    quote! {
        impl #struct_name {
            /// Creates a new instance of this service.
            #[allow(clippy::too_many_arguments)]
            pub fn new(#(#params),*) -> Self {
                Self {
                    #(#field_inits,)*
                    #emitter_init
                    #view_init
                    __rillet_cancel_token: rillet::runtime::CancellationToken::new(),
                    __rillet_join_handles: rillet::runtime::Arc::new(rillet::runtime::Mutex::new(Vec::new())),
                    __rillet_sampling_state: rillet::metrics::SamplingState::new(),
                }
            }

            /// Signals cancellation to the service loop and its tasks.
            ///
            /// The returned completion waits for them to finish.
            pub fn cancel(&self) -> rillet::runtime::TaskCompletion {
                self.__rillet_cancel_token.cancel();
                rillet::runtime::TaskCompletion::new(self.__rillet_join_handles.clone())
            }
        }
    }
}

/// Generate emit_<event>() methods and the __rillet_wire_emitter helper.
fn generate_emit_methods(
    struct_name: &Ident,
    emitted_events: &[Ident],
    event_capacity: usize,
) -> TokenStream {
    if emitted_events.is_empty() {
        // The wire helper is a no-op without events, so the generated spawn
        // can always call it.
        return quote! {
            impl #struct_name {
                #[doc(hidden)]
                pub fn __rillet_wire_emitter(&mut self) {}
            }
        };
    }

    let emit_methods: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            let method_name = format_ident!("emit_{}", to_snake_case(&event.to_string()));

            quote! {
                /// Emits the event to all subscribers.
                fn #method_name(&self, event: #event) {
                    self.__rillet_emitter.emit(event);
                }
            }
        })
        .collect();

    let channel_creation: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            quote! {
                builder.add_event::<#event>(#event_capacity);
            }
        })
        .collect();

    quote! {
        impl #struct_name {
            #(#emit_methods)*

            /// Creates the event channels. Called by the generated spawn.
            #[doc(hidden)]
            pub fn __rillet_wire_emitter(&mut self) {
                let mut builder = rillet::event::EmitterBuilder::new();
                #(#channel_creation)*
                self.__rillet_emitter = builder.build();
            }
        }
    }
}

/// Generate the view publication plumbing and view handle type.
///
/// The seed/publish helpers exist on every service (as no-ops without a
/// view), so the generated spawn loop can call them unconditionally.
fn generate_view_impls(
    struct_name: &Ident,
    handle_name: &Ident,
    view_type: Option<&Type>,
    emitted_events: &[Ident],
) -> TokenStream {
    let Some(view_ty) = view_type else {
        return quote! {
            impl #struct_name {
                #[doc(hidden)]
                pub fn __rillet_seed_view(&mut self) {}

                #[doc(hidden)]
                pub fn __rillet_view_slot_any(
                    &self,
                ) -> Option<rillet::runtime::Arc<dyn std::any::Any + Send + Sync>> {
                    None
                }

                #[doc(hidden)]
                pub fn __rillet_publish_view(&self) {}
            }
        };
    };

    let view_handle_name = format_ident!("{}ViewHandle", struct_name);

    // The view handle re-exposes event subscriptions, but no commands,
    // getters, or direct methods.
    let subscription_delegations: Vec<TokenStream> = emitted_events
        .iter()
        .map(|event| {
            let method_name = format_ident!("on_{}", to_snake_case(&event.to_string()));

            quote! {
                /// Subscribe to this event type.
                pub fn #method_name(&self) -> rillet::event::EventReceiver<#event> {
                    self.inner.#method_name()
                }
            }
        })
        .collect();

    quote! {
        impl #struct_name {
            /// Seeds the view slot from the initial state.
            #[doc(hidden)]
            pub fn __rillet_seed_view(&mut self) {
                self.__rillet_view_slot = Some(rillet::view::ViewSlot::new(self.view()));
            }

            #[doc(hidden)]
            pub fn __rillet_view_slot_any(
                &self,
            ) -> Option<rillet::runtime::Arc<dyn std::any::Any + Send + Sync>> {
                self.__rillet_view_slot.as_ref().map(|slot| {
                    rillet::runtime::Arc::new(slot.clone())
                        as rillet::runtime::Arc<dyn std::any::Any + Send + Sync>
                })
            }

            /// Recomputes the view and publishes it if it changed.
            ///
            /// Callers hold the state write lock.
            #[doc(hidden)]
            pub fn __rillet_publish_view(&self) {
                if let Some(slot) = &self.__rillet_view_slot {
                    slot.publish(self.view());
                }
            }
        }

        impl #handle_name {
            #[doc(hidden)]
            fn __rillet_slot(&self) -> &rillet::view::ViewSlot<#view_ty> {
                self.__rillet_view
                    .as_ref()
                    .and_then(|slot| slot.downcast_ref::<rillet::view::ViewSlot<#view_ty>>())
                    .expect("view slot not seeded at spawn")
            }

            /// Returns the latest published view without taking any lock.
            pub fn view(&self) -> rillet::runtime::Arc<#view_ty> {
                self.__rillet_slot().load()
            }

            /// Returns a watcher that has already seen the current view.
            pub fn watch_view(&self) -> rillet::view::ViewWatcher<#view_ty> {
                self.__rillet_slot().watch()
            }
        }

        /// A read-only handle to the service: its view and its events.
        ///
        /// It exposes no commands, getters, or direct methods, and takes no
        /// locks.
        #[derive(Clone)]
        pub struct #view_handle_name {
            inner: #handle_name,
        }

        impl From<#handle_name> for #view_handle_name {
            fn from(handle: #handle_name) -> Self {
                Self { inner: handle }
            }
        }

        impl #view_handle_name {
            /// Returns the latest published view without taking any lock.
            pub fn view(&self) -> rillet::runtime::Arc<#view_ty> {
                self.inner.view()
            }

            /// Returns a watcher that has already seen the current view.
            pub fn watch_view(&self) -> rillet::view::ViewWatcher<#view_ty> {
                self.inner.watch_view()
            }

            #(#subscription_delegations)*
        }
    }
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Generate cancel(), task_completion(), and cancel_token() methods on the handle.
fn generate_shutdown_method(handle_name: &Ident) -> TokenStream {
    quote! {
        impl #handle_name {
            /// Returns the service's cancellation token.
            pub fn cancel_token(&self) -> rillet::runtime::CancellationToken {
                self.state.read().unwrap().__rillet_cancel_token.clone()
            }

            /// Signals cancellation to the service loop and its tasks.
            ///
            /// The returned completion waits for them to finish.
            pub fn cancel(&self) -> rillet::runtime::TaskCompletion {
                self.state.read().unwrap().__rillet_cancel_token.cancel();
                self.task_completion()
            }

            /// Returns a completion for the service's tasks without
            /// cancelling them.
            pub fn task_completion(&self) -> rillet::runtime::TaskCompletion {
                rillet::runtime::TaskCompletion::new(self.state.read().unwrap().__rillet_join_handles.clone())
            }
        }
    }
}
