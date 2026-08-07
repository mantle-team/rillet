//! Implementation of the `#[rillet::handlers]` attribute macro.
//!
//! From an impl block's annotated methods, the macro generates:
//! - the command enum and queueing methods for `#[rillet(command)]`
//! - handle wrappers for `#[rillet(direct)]` and `#[rillet(direct_mut)]`
//! - event subscriptions for `#[rillet(from = field)]`
//! - view watches for `#[rillet(watch = field)]`
//! - the spawn methods, whose loop selects over commands, events, and watches
//!
//! Every mutation the macro generates (commands, event handlers, direct_mut
//! calls) republishes the service's view while still holding the write lock,
//! so views can never tear against the state that produced them.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::case::{to_pascal_case, to_snake_case};
use syn::{Error, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, Pat, Result, Type, parse2};

/// A parsed command method.
struct CommandMethod {
    method_name: Ident,
    variant_name: Ident,
    params: Vec<(Ident, Type)>,
}

/// A parsed direct or direct_mut method.
struct DirectMethod {
    method_name: Ident,
    params: Vec<(Ident, Type)>,
    return_type: syn::ReturnType,
}

/// A parsed event handler method.
struct EventHandler {
    method_name: Ident,
    /// The field whose events drive the handler.
    source_field: Ident,
    /// The event type, taken from the method's parameter.
    event_type: Type,
}

/// A parsed view watch handler method.
struct WatchHandler {
    method_name: Ident,
    /// The field whose view publications drive the handler.
    source_field: Ident,
}

/// A parsed background task method.
struct TaskMethod {
    method_name: Ident,
    /// Parameters beyond the handle and the cancellation token.
    extra_params: Vec<(Ident, Type)>,
}

/// Expands `#[rillet::handlers]`.
pub fn expand(_attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let impl_block: ItemImpl = parse2(item)?;

    let struct_name = match &*impl_block.self_ty {
        Type::Path(type_path) => type_path.path.get_ident().cloned().ok_or_else(|| {
            Error::new_spanned(&impl_block.self_ty, "expected a simple type name")
        })?,
        _ => {
            return Err(Error::new_spanned(
                &impl_block.self_ty,
                "expected a simple type name",
            ));
        }
    };

    let command_enum_name = format_ident!("{}Command", struct_name);
    let handle_name = format_ident!("{}Handle", struct_name);

    let mut command_methods: Vec<CommandMethod> = Vec::new();
    let mut direct_methods: Vec<DirectMethod> = Vec::new();
    let mut direct_mut_methods: Vec<DirectMethod> = Vec::new();
    let mut event_handlers: Vec<EventHandler> = Vec::new();
    let mut watch_handlers: Vec<WatchHandler> = Vec::new();
    let mut task_methods: Vec<TaskMethod> = Vec::new();
    let mut clean_items: Vec<ImplItem> = Vec::new();

    for item in impl_block.items.iter() {
        if let ImplItem::Fn(method) = item {
            let attrs = parse_method_attrs(method)?;

            if attrs.is_command {
                command_methods.push(CommandMethod {
                    method_name: method.sig.ident.clone(),
                    variant_name: format_ident!(
                        "{}",
                        to_pascal_case(&method.sig.ident.to_string())
                    ),
                    params: extract_all_params(&method.sig.inputs)?,
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if attrs.is_direct {
                direct_methods.push(DirectMethod {
                    method_name: method.sig.ident.clone(),
                    params: extract_all_params(&method.sig.inputs)?,
                    return_type: method.sig.output.clone(),
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if attrs.is_direct_mut {
                direct_mut_methods.push(DirectMethod {
                    method_name: method.sig.ident.clone(),
                    params: extract_all_params(&method.sig.inputs)?,
                    return_type: method.sig.output.clone(),
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if let Some(source_field) = attrs.from_field {
                let (_, event_type) = extract_param(&method.sig.inputs)?.ok_or_else(|| {
                    Error::new_spanned(method, "event handler must have an event parameter")
                })?;
                event_handlers.push(EventHandler {
                    method_name: method.sig.ident.clone(),
                    source_field,
                    event_type,
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if let Some(source_field) = attrs.watch_field {
                watch_handlers.push(WatchHandler {
                    method_name: method.sig.ident.clone(),
                    source_field,
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if attrs.is_task {
                let all_params = extract_all_params(&method.sig.inputs)?;
                // The first two parameters are the handle and the
                // cancellation token.
                let extra_params = all_params.into_iter().skip(2).collect();
                task_methods.push(TaskMethod {
                    method_name: method.sig.ident.clone(),
                    extra_params,
                });
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else {
                clean_items.push(item.clone());
            }
        } else {
            clean_items.push(item.clone());
        }
    }

    let cmd_count = command_methods.len();
    let (command_enum, execute_impl, handle_sender_impl, metrics_impl) = generate_command_infra(
        &struct_name,
        &command_enum_name,
        &handle_name,
        &command_methods,
    );

    // The handle struct is generated here, not in service.rs: the metrics
    // field needs the command count.
    let handle_struct = quote! {
        /// Handle for interacting with the service.
        #[derive(Clone)]
        pub struct #handle_name {
            state: rillet::runtime::Arc<rillet::runtime::RwLock<#struct_name>>,
            cmd_tx: rillet::runtime::mpsc::Sender<#command_enum_name>,
            metrics: rillet::runtime::Arc<rillet::metrics::CommandMetrics<#cmd_count>>,
            __rillet_view: Option<rillet::runtime::Arc<dyn std::any::Any + Send + Sync>>,
        }
    };

    let spawn_impl = generate_spawn_impl(
        &struct_name,
        &handle_name,
        &command_enum_name,
        &command_methods,
        &event_handlers,
        &watch_handlers,
        &task_methods,
    );

    let direct_impl = generate_direct_methods(&struct_name, &handle_name, &direct_methods);
    let direct_mut_impl =
        generate_direct_mut_methods(&struct_name, &handle_name, &direct_mut_methods);

    let impl_generics = &impl_block.generics;
    let where_clause = &impl_block.generics.where_clause;

    let clean_impl = quote! {
        impl #impl_generics #struct_name #where_clause {
            #(#clean_items)*
        }
    };

    Ok(quote! {
        #command_enum
        #execute_impl
        #handle_struct
        #clean_impl
        #handle_sender_impl
        #spawn_impl
        #direct_impl
        #direct_mut_impl
        #metrics_impl
    })
}

// ============================================================================
// Parsing
// ============================================================================

/// Parsed rillet attributes from a method.
struct MethodAttrs {
    is_command: bool,
    is_direct: bool,
    is_direct_mut: bool,
    is_task: bool,
    from_field: Option<Ident>,
    watch_field: Option<Ident>,
}

fn parse_method_attrs(method: &ImplItemFn) -> Result<MethodAttrs> {
    let mut attrs = MethodAttrs {
        is_command: false,
        is_direct: false,
        is_direct_mut: false,
        is_task: false,
        from_field: None,
        watch_field: None,
    };

    for attr in &method.attrs {
        if !attr.path().is_ident("rillet") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("command") {
                attrs.is_command = true;
            } else if meta.path.is_ident("direct") {
                attrs.is_direct = true;
            } else if meta.path.is_ident("direct_mut") {
                attrs.is_direct_mut = true;
            } else if meta.path.is_ident("task") {
                attrs.is_task = true;
            } else if meta.path.is_ident("from") {
                meta.input.parse::<syn::Token![=]>()?;
                let field: Ident = meta.input.parse()?;
                attrs.from_field = Some(field);
            } else if meta.path.is_ident("watch") {
                meta.input.parse::<syn::Token![=]>()?;
                let field: Ident = meta.input.parse()?;
                attrs.watch_field = Some(field);
            } else {
                return Err(meta.error(
                    "unknown rillet attribute; expected `command`, `direct`, `direct_mut`, \
                     `task`, `from = <field>`, or `watch = <field>`",
                ));
            }
            Ok(())
        })?;
    }

    Ok(attrs)
}

fn extract_param(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
) -> Result<Option<(Ident, Type)>> {
    for input in inputs.iter() {
        if let FnArg::Typed(pat_type) = input {
            let param_name = match &*pat_type.pat {
                Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "rillet handler parameters must be simple identifiers",
                    ));
                }
            };
            let param_type = (*pat_type.ty).clone();
            return Ok(Some((param_name, param_type)));
        }
    }
    Ok(None)
}

fn extract_all_params(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
) -> Result<Vec<(Ident, Type)>> {
    let mut params = Vec::new();
    for input in inputs.iter() {
        if let FnArg::Typed(pat_type) = input {
            let param_name = match &*pat_type.pat {
                Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "rillet handler parameters must be simple identifiers",
                    ));
                }
            };
            let param_type = (*pat_type.ty).clone();
            params.push((param_name, param_type));
        }
    }
    Ok(params)
}

fn strip_rillet_attrs(mut method: ImplItemFn) -> ImplItemFn {
    method.attrs.retain(|attr| !attr.path().is_ident("rillet"));
    method
}

/// Derives the subscription method name from the event type, MessageSent
/// becoming on_message_sent.
fn derive_subscription_method(event_type: &Type) -> Ident {
    let type_name = match event_type {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };

    let snake_name = to_snake_case(&type_name);
    format_ident!("on_{}", snake_name)
}

// ============================================================================
// Code Generation
// ============================================================================

fn generate_command_infra(
    struct_name: &Ident,
    command_enum_name: &Ident,
    handle_name: &Ident,
    commands: &[CommandMethod],
) -> (TokenStream, TokenStream, TokenStream, TokenStream) {
    let cmd_count = commands.len();

    let enum_variants: Vec<TokenStream> = commands
        .iter()
        .map(|cmd| {
            let variant = &cmd.variant_name;
            if cmd.params.is_empty() {
                quote! { #variant, }
            } else {
                let types: Vec<_> = cmd.params.iter().map(|(_, ty)| ty).collect();
                quote! { #variant(#(#types),*), }
            }
        })
        .collect();

    let index_arms: Vec<TokenStream> = commands
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let variant = &cmd.variant_name;
            if cmd.params.is_empty() {
                quote! { Self::#variant => #i, }
            } else {
                quote! { Self::#variant(..) => #i, }
            }
        })
        .collect();

    let name_arms: Vec<TokenStream> = commands
        .iter()
        .map(|cmd| {
            let variant = &cmd.variant_name;
            let name = cmd.method_name.to_string();
            if cmd.params.is_empty() {
                quote! { Self::#variant => #name, }
            } else {
                quote! { Self::#variant(..) => #name, }
            }
        })
        .collect();

    let names: Vec<String> = commands
        .iter()
        .map(|cmd| cmd.method_name.to_string())
        .collect();

    let index_body = if commands.is_empty() {
        quote! { unreachable!("empty command enum cannot be instantiated") }
    } else {
        quote! {
            match self {
                #(#index_arms)*
            }
        }
    };

    let name_body = if commands.is_empty() {
        quote! { unreachable!("empty command enum cannot be instantiated") }
    } else {
        quote! {
            match self {
                #(#name_arms)*
            }
        }
    };

    let command_enum = quote! {
        #[allow(clippy::enum_variant_names, clippy::module_name_repetitions)]
        enum #command_enum_name {
            #(#enum_variants)*
        }

        impl #command_enum_name {
            /// Number of command variants.
            const COUNT: usize = #cmd_count;

            /// Array of command names for metrics.
            const NAMES: [&'static str; #cmd_count] = [#(#names),*];

            /// Returns the index of this command variant.
            #[inline]
            fn index(&self) -> usize {
                #index_body
            }

            /// Returns the name of this command variant.
            #[inline]
            fn name(&self) -> &'static str {
                #name_body
            }
        }
    };

    let match_arms: Vec<TokenStream> = commands
        .iter()
        .map(|cmd| {
            let variant = &cmd.variant_name;
            let method = &cmd.method_name;
            let param_names: Vec<_> = cmd
                .params
                .iter()
                .enumerate()
                .map(|(i, _)| format_ident!("v{}", i))
                .collect();

            if cmd.params.is_empty() {
                quote! { #command_enum_name::#variant => state.#method(), }
            } else {
                quote! { #command_enum_name::#variant(#(#param_names),*) => state.#method(#(#param_names),*), }
            }
        })
        .collect();

    let execute_impl = quote! {
        impl #command_enum_name {
            fn execute(self, state: &mut #struct_name) {
                match self {
                    #(#match_arms)*
                }
            }
        }
    };

    let sender_methods: Vec<TokenStream> = commands
        .iter()
        .enumerate()
        .map(|(idx, cmd)| {
            let method = &cmd.method_name;
            let variant = &cmd.variant_name;
            if cmd.params.is_empty() {
                quote! {
                    pub fn #method(&self) {
                        self.metrics.inc_enqueued(#idx);
                        rillet::runtime::send_command(
                            &self.cmd_tx,
                            #command_enum_name::#variant,
                            stringify!(#method),
                        );
                    }
                }
            } else {
                let param_decls: Vec<TokenStream> = cmd
                    .params
                    .iter()
                    .map(|(name, ty)| quote! { #name: #ty })
                    .collect();
                let param_names: Vec<_> = cmd.params.iter().map(|(name, _)| name).collect();
                quote! {
                    pub fn #method(&self, #(#param_decls),*) {
                        self.metrics.inc_enqueued(#idx);
                        rillet::runtime::send_command(
                            &self.cmd_tx,
                            #command_enum_name::#variant(#(#param_names),*),
                            stringify!(#method),
                        );
                    }
                }
            }
        })
        .collect();

    let handle_impl = quote! {
        impl #handle_name {
            #(#sender_methods)*
        }
    };

    let metrics_impl = quote! {
        impl #handle_name {
            /// Returns per-command statistics.
            pub fn command_stats(&self) -> impl Iterator<Item = rillet::metrics::CommandStats> + '_ {
                (0..#cmd_count).map(move |i| rillet::metrics::CommandStats {
                    name: #command_enum_name::NAMES[i],
                    depth: self.metrics.command_depth(i),
                    total_enqueued: self.metrics.command_enqueued(i),
                    total_processed: self.metrics.command_processed(i),
                })
            }

            /// Returns aggregate statistics computed from depth samples.
            pub fn aggregate_stats(&self) -> rillet::metrics::AggregateStats {
                self.metrics.aggregate_stats()
            }
        }
    };

    (command_enum, execute_impl, handle_impl, metrics_impl)
}

fn generate_spawn_impl(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    commands: &[CommandMethod],
    event_handlers: &[EventHandler],
    watch_handlers: &[WatchHandler],
    tasks: &[TaskMethod],
) -> TokenStream {
    // A task with extra parameters needs the builder to carry them.
    let has_context_tasks = tasks.iter().any(|t| !t.extra_params.is_empty());

    let mut event_receiver_setup: Vec<TokenStream> = event_handlers
        .iter()
        .map(|handler| {
            let field = &handler.source_field;
            let handler_method = &handler.method_name;
            let rx_name = format_ident!("{}_rx", handler_method);
            let subscription_method = derive_subscription_method(&handler.event_type);
            quote! {
                let mut #rx_name = self.#field.#subscription_method();
            }
        })
        .collect();
    event_receiver_setup.extend(watch_handlers.iter().map(|handler| {
        let field = &handler.source_field;
        let rx_name = format_ident!("{}_rx", handler.method_name);
        quote! {
            let mut #rx_name = self.#field.watch_view();
        }
    }));

    // One flag per event source, recording whether the source is still
    // open.
    let event_open_flags: Vec<Ident> = event_handlers
        .iter()
        .map(|handler| format_ident!("{}_open", handler.method_name))
        .collect();

    // Whether any input source is still open. A watch source is always
    // open: its slot has no closed state.
    let inputs_open_expr = {
        let mut terms: Vec<TokenStream> = event_open_flags
            .iter()
            .map(|flag| quote! { #flag })
            .collect();
        if !watch_handlers.is_empty() {
            terms.push(quote! { true });
        }
        if terms.is_empty() {
            quote! { false }
        } else {
            quote! { #(#terms)||* }
        }
    };
    let exit_check = quote! {
        if !cmd_open {
            let inputs_open = #inputs_open_expr;
            if !inputs_open || !state.read().unwrap().__rillet_has_observers() {
                break;
            }
        }
    };

    // In each event and watch arm, the handler and the view republication
    // run under one write lock acquisition.
    let mut event_select_arms: Vec<TokenStream> = event_handlers
        .iter()
        .map(|handler| {
            let method = &handler.method_name;
            let fut_name = format_ident!("{}_rx_fut", handler.method_name);
            let open_name = format_ident!("{}_open", handler.method_name);
            quote! {
                result = #fut_name => {
                    match result {
                        Some(event) => {
                            {
                                let mut s = state.write().unwrap();
                                s.#method(event);
                                s.__rillet_publish_view();
                            }
                            #exit_check
                        }
                        None => {
                            #open_name = false;
                            #exit_check
                        }
                    }
                }
            }
        })
        .collect();
    event_select_arms.extend(watch_handlers.iter().map(|handler| {
        let method = &handler.method_name;
        let fut_name = format_ident!("{}_rx_fut", handler.method_name);
        quote! {
            view = #fut_name => {
                {
                    let mut s = state.write().unwrap();
                    s.#method(view);
                    s.__rillet_publish_view();
                }
                #exit_check
            }
        }
    }));

    // The receive futures are recreated inside the loop, before the select;
    // futures::select! requires Unpin futures, hence the pinning.
    let mut event_fut_setup: Vec<TokenStream> = event_handlers
        .iter()
        .map(|handler| {
            let rx_name = format_ident!("{}_rx", handler.method_name);
            let fut_name = format_ident!("{}_rx_fut", handler.method_name);
            let open_name = format_ident!("{}_open", handler.method_name);
            let closed_name = format_ident!("{}_closed", handler.method_name);
            quote! {
                let #closed_name = !#open_name;
                let mut #fut_name = std::pin::pin!(
                    async {
                        if #closed_name {
                            std::future::pending().await
                        } else {
                            #rx_name.next().await
                        }
                    }
                    .fuse()
                );
            }
        })
        .collect();
    event_fut_setup.extend(watch_handlers.iter().map(|handler| {
        let rx_name = format_ident!("{}_rx", handler.method_name);
        let fut_name = format_ident!("{}_rx_fut", handler.method_name);
        quote! {
            let mut #fut_name = std::pin::pin!(#rx_name.changed().fuse());
        }
    }));

    let has_commands = !commands.is_empty();
    let has_events = !event_handlers.is_empty() || !watch_handlers.is_empty();

    // The command body and the view republication run under one write lock
    // acquisition, so the published view is the one this command produced.
    let cmd_process_with_metrics = quote! {
        let cmd_idx = cmd.index();
        {
            let mut s = state.write().unwrap();
            cmd.execute(&mut s);
            s.__rillet_publish_view();

            s.__rillet_sampling_state.inc_command();
            if s.__rillet_sampling_state.should_sample() {
                metrics.record_sample();
                s.__rillet_sampling_state.reset();
            }
        }
        metrics.inc_processed(cmd_idx);
    };

    // Drains the remaining commands on shutdown.
    let cmd_drain = quote! {
        while let Ok(cmd) = cmd_rx.try_recv() {
            #cmd_process_with_metrics
        }
    };

    // recv on a closed channel returns Err on every poll, so once the
    // channel closes the loop polls a pending future in its place.
    let cmd_recv_setup = quote! {
        let cmd_closed = !cmd_open;
        let mut cmd_fut = std::pin::pin!(
            async {
                if cmd_closed {
                    std::future::pending().await
                } else {
                    cmd_rx.recv().await
                }
            }
            .fuse()
        );
    };

    // With no event handlers the event interpolations expand to nothing
    // and the exit check reduces to breaking on command-channel close.
    let service_loop = if has_commands {
        quote! {
            let state = state_clone;
            let mut cmd_rx = cmd_rx;
            let mut cmd_open = true;
            #(let mut #event_open_flags = true;)*
            loop {
                use rillet::runtime::FutureExt;

                #(#event_fut_setup)*

                let mut cancel_fut = std::pin::pin!(cancel_token.cancelled().fuse());
                #cmd_recv_setup

                rillet::runtime::futures::select! {
                    _ = cancel_fut => {
                        // Drain remaining commands before exit
                        #cmd_drain
                        break;
                    }
                    result = cmd_fut => {
                        match result {
                            Ok(cmd) => {
                                #cmd_process_with_metrics
                            }
                            Err(_) => {
                                cmd_open = false;
                                #exit_check
                            }
                        }
                    }
                    #(#event_select_arms)*
                }
            }
        }
    } else if has_events {
        quote! {
            let state = state_clone;
            let cmd_rx = cmd_rx;
            let mut cmd_open = true;
            #(let mut #event_open_flags = true;)*
            loop {
                use rillet::runtime::FutureExt;

                #(#event_fut_setup)*

                let mut cancel_fut = std::pin::pin!(cancel_token.cancelled().fuse());
                #cmd_recv_setup

                rillet::runtime::futures::select! {
                    _ = cancel_fut => break,
                    result = cmd_fut => {
                        if result.is_err() {
                            cmd_open = false;
                            #exit_check
                        }
                    }
                    #(#event_select_arms)*
                }
            }
        }
    } else {
        quote! {
            let _state = state_clone;
            use rillet::runtime::FutureExt;

            let mut cancel_fut = std::pin::pin!(cancel_token.cancelled().fuse());
            let mut closed_fut = std::pin::pin!(
                async {
                    while cmd_rx.recv().await.is_ok() {}
                }
                .fuse()
            );

            rillet::runtime::futures::select! {
                _ = cancel_fut => {}
                _ = closed_fut => {}
            }
        }
    };

    let cmd_count = commands.len();

    if has_context_tasks {
        generate_spawn_with_builder(
            struct_name,
            handle_name,
            command_enum_name,
            cmd_count,
            tasks,
            &event_receiver_setup,
            &service_loop,
        )
    } else {
        generate_spawn_simple(
            struct_name,
            handle_name,
            command_enum_name,
            cmd_count,
            tasks,
            &event_receiver_setup,
            &service_loop,
        )
    }
}

/// Generate the `__rillet_spawn_core_with` method both spawn paths call:
/// it spawns the service loop and returns the handle and the shared task
/// handles.
fn generate_spawn_core(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    cmd_count: usize,
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    quote! {
        impl #struct_name {
            /// Spawns the service loop; the caller spawns the tasks.
            fn __rillet_spawn_core_with<__S: rillet::runtime::Spawner>(mut self, spawner: &__S) -> (#handle_name, rillet::runtime::Arc<rillet::runtime::Mutex<Vec<Box<dyn rillet::runtime::TaskHandle>>>>) {
                let (cmd_tx, cmd_rx) = rillet::runtime::mpsc::bounded::<#command_enum_name>(#struct_name::__RILLET_COMMAND_CAPACITY);

                let metrics = rillet::runtime::Arc::new(
                    rillet::metrics::CommandMetrics::<#cmd_count>::new()
                );

                self.__rillet_seed_view();
                let __rillet_view = self.__rillet_view_slot_any();

                #(#event_receiver_setup)*

                let cancel_token = self.__rillet_cancel_token.clone();
                let join_handles = self.__rillet_join_handles.clone();

                let state = rillet::runtime::Arc::new(rillet::runtime::RwLock::new(self));
                let state_clone = state.clone();

                {
                    let join_handles = join_handles.clone();
                    let cancel_token = cancel_token.clone();
                    let metrics = metrics.clone();
                    let main_loop_handle = spawner.spawn(async move {
                        #service_loop
                    });
                    join_handles.lock().unwrap().push(Box::new(main_loop_handle));
                }

                (
                    #handle_name {
                        state,
                        cmd_tx,
                        metrics,
                        __rillet_view,
                    },
                    join_handles,
                )
            }
        }
    }
}

/// Generate the spawn of one handle-only task.
fn generate_handle_task_spawn(struct_name: &Ident, method_name: &Ident) -> TokenStream {
    quote! {
        {
            let handle = handle.clone();
            let cancel_token = cancel_token.clone();
            let task_handle = spawner.spawn(async move {
                #struct_name::#method_name(handle, cancel_token).await;
            });
            join_handles.lock().unwrap().push(Box::new(task_handle));
        }
    }
}

/// Generate the plain spawn methods, used when every task is handle-only.
fn generate_spawn_simple(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    cmd_count: usize,
    tasks: &[TaskMethod],
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    let spawn_core = generate_spawn_core(
        struct_name,
        handle_name,
        command_enum_name,
        cmd_count,
        event_receiver_setup,
        service_loop,
    );

    let task_spawns: Vec<TokenStream> = tasks
        .iter()
        .map(|task| generate_handle_task_spawn(struct_name, &task.method_name))
        .collect();

    // Without tasks, the token and handle bindings would be unused.
    let task_setup = if tasks.is_empty() {
        quote! {
            let (handle, _) = self.__rillet_spawn_core_with(&spawner);
        }
    } else {
        quote! {
            let cancel_token = self.__rillet_cancel_token.clone();
            let (handle, join_handles) = self.__rillet_spawn_core_with(&spawner);
        }
    };

    quote! {
        #spawn_core

        impl #struct_name {
            /// Spawns this service on the default [`SmolSpawner`](rillet::SmolSpawner)
            /// and returns its handle.
            ///
            /// The service runs until cancelled.
            pub fn spawn(self) -> #handle_name {
                self.spawn_with(rillet::runtime::SmolSpawner)
            }

            /// Spawns this service on the given spawner and returns its
            /// handle.
            ///
            /// The service runs until cancelled.
            pub fn spawn_with<__S: rillet::runtime::Spawner>(self, spawner: __S) -> #handle_name {
                #task_setup

                #(#task_spawns)*

                handle
            }
        }
    }
}

/// Generate the spawn builder, used when any task has extra parameters.
fn generate_spawn_with_builder(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    cmd_count: usize,
    tasks: &[TaskMethod],
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    let builder_name = format_ident!("__{struct_name}Builder");

    let builder_fields: Vec<TokenStream> = tasks
        .iter()
        .filter(|t| !t.extra_params.is_empty())
        .map(|task| {
            let field_name = format_ident!("{}_ctx", task.method_name);
            let types: Vec<_> = task.extra_params.iter().map(|(_, ty)| ty).collect();
            quote! {
                #field_name: Option<(#(#types,)*)>
            }
        })
        .collect();

    // The service's spawn_<task> methods each return the builder.
    let service_spawn_methods: Vec<TokenStream> = tasks
        .iter()
        .filter(|t| !t.extra_params.is_empty())
        .map(|task| {
            let method_name = format_ident!("spawn_{}", task.method_name);
            let field_name = format_ident!("{}_ctx", task.method_name);
            let param_decls: Vec<TokenStream> = task
                .extra_params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();
            let param_names: Vec<_> = task.extra_params.iter().map(|(name, _)| name).collect();

            // The other context fields start as None.
            let other_inits: Vec<TokenStream> = tasks
                .iter()
                .filter(|t| !t.extra_params.is_empty() && t.method_name != task.method_name)
                .map(|t| {
                    let fname = format_ident!("{}_ctx", t.method_name);
                    quote! { #fname: None }
                })
                .collect();

            quote! {
                pub fn #method_name(self, #(#param_decls),*) -> #builder_name {
                    #builder_name {
                        inner: self,
                        #field_name: Some((#(#param_names,)*)),
                        #(#other_inits,)*
                    }
                }
            }
        })
        .collect();

    // The builder's spawn_<task> methods chain.
    let builder_spawn_methods: Vec<TokenStream> = tasks
        .iter()
        .filter(|t| !t.extra_params.is_empty())
        .map(|task| {
            let method_name = format_ident!("spawn_{}", task.method_name);
            let field_name = format_ident!("{}_ctx", task.method_name);
            let param_decls: Vec<TokenStream> = task
                .extra_params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();
            let param_names: Vec<_> = task.extra_params.iter().map(|(name, _)| name).collect();

            quote! {
                pub fn #method_name(mut self, #(#param_decls),*) -> Self {
                    self.#field_name = Some((#(#param_names,)*));
                    self
                }
            }
        })
        .collect();

    // The builder's spawn_with spawns both context and handle-only tasks.
    let task_spawns: Vec<TokenStream> = tasks
        .iter()
        .map(|task| {
            let method_name = &task.method_name;
            if task.extra_params.is_empty() {
                generate_handle_task_spawn(struct_name, method_name)
            } else {
                // A context task takes its parameters from the builder.
                let field_name = format_ident!("{}_ctx", task.method_name);
                let param_names: Vec<_> = task
                    .extra_params
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format_ident!("__ctx_{}", i))
                    .collect();

                quote! {
                    if let Some((#(#param_names,)*)) = self.#field_name {
                        let handle = handle.clone();
                        let cancel_token = cancel_token.clone();
                        let task_handle = spawner.spawn(async move {
                            #struct_name::#method_name(handle, cancel_token, #(#param_names),*).await;
                        });
                        join_handles.lock().unwrap().push(Box::new(task_handle));
                    }
                }
            }
        })
        .collect();

    let spawn_core = generate_spawn_core(
        struct_name,
        handle_name,
        command_enum_name,
        cmd_count,
        event_receiver_setup,
        service_loop,
    );

    quote! {
        #[doc(hidden)]
        pub struct #builder_name {
            inner: #struct_name,
            #(#builder_fields,)*
        }

        #spawn_core

        impl #struct_name {
            #(#service_spawn_methods)*
        }

        impl #builder_name {
            #(#builder_spawn_methods)*

            /// Spawns this service on the default [`SmolSpawner`](rillet::SmolSpawner)
            /// and returns its handle.
            ///
            /// The service runs until cancelled.
            pub fn spawn(self) -> #handle_name {
                self.spawn_with(rillet::runtime::SmolSpawner)
            }

            /// Spawns this service on the given spawner and returns its
            /// handle.
            ///
            /// The service runs until cancelled.
            pub fn spawn_with<__S: rillet::runtime::Spawner>(self, spawner: __S) -> #handle_name {
                let cancel_token = self.inner.__rillet_cancel_token.clone();
                let (handle, join_handles) = self.inner.__rillet_spawn_core_with(&spawner);

                #(#task_spawns)*

                handle
            }
        }
    }
}

/// Generate the handle wrappers for direct methods, which call the service
/// method under the read lock.
fn generate_direct_methods(
    _struct_name: &Ident,
    handle_name: &Ident,
    methods: &[DirectMethod],
) -> TokenStream {
    if methods.is_empty() {
        return quote! {};
    }

    let handle_methods: Vec<TokenStream> = methods
        .iter()
        .map(|method| {
            let method_name = &method.method_name;
            let return_type = &method.return_type;

            let param_decls: Vec<TokenStream> = method
                .params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();

            let param_names: Vec<&Ident> = method.params.iter().map(|(name, _)| name).collect();

            quote! {
                /// Runs on the caller's thread under the service's read lock.
                pub fn #method_name(&self, #(#param_decls),*) #return_type {
                    self.state.read().unwrap().#method_name(#(#param_names),*)
                }
            }
        })
        .collect();

    quote! {
        impl #handle_name {
            #(#handle_methods)*
        }
    }
}

/// Generate the handle wrappers for direct_mut methods, which call the
/// service method under the write lock and republish the view before
/// releasing it.
fn generate_direct_mut_methods(
    _struct_name: &Ident,
    handle_name: &Ident,
    methods: &[DirectMethod],
) -> TokenStream {
    if methods.is_empty() {
        return quote! {};
    }

    let handle_methods: Vec<TokenStream> = methods
        .iter()
        .map(|method| {
            let method_name = &method.method_name;
            let return_type = &method.return_type;

            let param_decls: Vec<TokenStream> = method
                .params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();

            let param_names: Vec<&Ident> = method.params.iter().map(|(name, _)| name).collect();

            quote! {
                /// Runs on the caller's thread under the service's write
                /// lock, republishing the view before returning.
                pub fn #method_name(&self, #(#param_decls),*) #return_type {
                    let mut s = self.state.write().unwrap();
                    let result = s.#method_name(#(#param_names),*);
                    s.__rillet_publish_view();
                    result
                }
            }
        })
        .collect();

    quote! {
        impl #handle_name {
            #(#handle_methods)*
        }
    }
}
