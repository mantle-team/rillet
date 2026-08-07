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
use syn::{Error, Expr, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, Pat, Result, Type, parse2};

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
    /// The upstream handle's subscription method, derived from the event
    /// type name.
    subscription_method: Ident,
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

/// A parsed op handler: a command whose handle method returns an `Op`
/// carrying its eventual outcome.
struct OpMethod {
    method_name: Ident,
    variant_name: Ident,
    params: Vec<(Ident, Type)>,
    reason_ty: Type,
    kind: OpKind,
}

/// How an op concludes: the handler returns the outcome itself, or the
/// outcome arrives later and is delivered by key.
enum OpKind {
    Immediate,
    Deferred(Box<DeferredOp>),
}

/// The declaration of an op whose outcome arrives later: the method that
/// executes it, the reason expiry fails it with, and the type of the key
/// that correlates its outcome.
struct DeferredOp {
    execute: Ident,
    timeout: Expr,
    key_ty: Type,
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
    let mut op_methods: Vec<OpMethod> = Vec::new();
    let mut clean_items: Vec<ImplItem> = Vec::new();

    for item in impl_block.items.iter() {
        if let ImplItem::Fn(method) = item {
            let attrs = parse_method_attrs(method)?;

            if let Some(op_attr) = attrs.op {
                op_methods.push(parse_op_method(method, op_attr)?);
                clean_items.push(ImplItem::Fn(strip_rillet_attrs(method.clone())));
            } else if attrs.is_command {
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
                let (_, event_type) = extract_all_params(&method.sig.inputs)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        Error::new_spanned(method, "event handler must have an event parameter")
                    })?;
                event_handlers.push(EventHandler {
                    method_name: method.sig.ident.clone(),
                    source_field,
                    subscription_method: derive_subscription_method(&event_type)?,
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

    let cmd_count = command_methods.len() + op_methods.len();
    let (command_enum, execute_impl, handle_sender_impl, metrics_impl) = generate_command_infra(
        &struct_name,
        &command_enum_name,
        &handle_name,
        &command_methods,
        &op_methods,
    );
    let op_conclusions = generate_op_conclusions(&struct_name, &op_methods);

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
            __rillet_cancel_token: rillet::runtime::CancellationToken,
            __rillet_join_handles: rillet::runtime::Arc<rillet::runtime::TaskSet>,
        }
    };

    let has_commands = !command_methods.is_empty() || !op_methods.is_empty();
    let has_deferred_ops = op_methods
        .iter()
        .any(|op| matches!(op.kind, OpKind::Deferred(_)));
    let spawn_impl = generate_spawn_impl(
        &struct_name,
        &handle_name,
        &command_enum_name,
        has_commands,
        cmd_count,
        has_deferred_ops,
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
        #op_conclusions
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
    op: Option<OpAttr>,
}

/// The parsed arguments of an `op` attribute.
struct OpAttr {
    execute: Option<Ident>,
    timeout: Option<Expr>,
}

fn parse_method_attrs(method: &ImplItemFn) -> Result<MethodAttrs> {
    let mut attrs = MethodAttrs {
        is_command: false,
        is_direct: false,
        is_direct_mut: false,
        is_task: false,
        from_field: None,
        watch_field: None,
        op: None,
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
            } else if meta.path.is_ident("op") {
                let mut op_attr = OpAttr {
                    execute: None,
                    timeout: None,
                };
                if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|op_meta| {
                        if op_meta.path.is_ident("execute") {
                            op_meta.input.parse::<syn::Token![=]>()?;
                            op_attr.execute = Some(op_meta.input.parse()?);
                        } else if op_meta.path.is_ident("timeout") {
                            op_meta.input.parse::<syn::Token![=]>()?;
                            op_attr.timeout = Some(op_meta.input.parse()?);
                        } else {
                            return Err(op_meta.error(
                                "unknown op argument; expected `execute = <method>` or \
                                 `timeout = <expr>`",
                            ));
                        }
                        Ok(())
                    })?;
                }
                attrs.op = Some(op_attr);
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
                     `task`, `op`, `from = <field>`, or `watch = <field>`",
                ));
            }
            Ok(())
        })?;
    }

    let claimed: Vec<&str> = [
        (attrs.is_command, "command"),
        (attrs.is_direct, "direct"),
        (attrs.is_direct_mut, "direct_mut"),
        (attrs.is_task, "task"),
        (attrs.op.is_some(), "op"),
        (attrs.from_field.is_some(), "from"),
        (attrs.watch_field.is_some(), "watch"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    if claimed.len() > 1 {
        return Err(Error::new_spanned(
            &method.sig,
            format!(
                "a rillet method can have only one role; found `{}`",
                claimed.join("`, `")
            ),
        ));
    }

    Ok(attrs)
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

/// Parses an op method.
///
/// A method declaring `execute = <method>` describes an outcome that
/// arrives later: it is an associated fn, run on the caller's thread,
/// returning the `Start<K, R>` that names the operation's key and
/// deadline. A method without it produces the outcome itself: an ordinary
/// handler returning `Result<(), R>`, whose return value concludes the
/// operation.
fn parse_op_method(method: &ImplItemFn, attr: OpAttr) -> Result<OpMethod> {
    let method_name = method.sig.ident.clone();
    let variant_name = format_ident!("{}", to_pascal_case(&method_name.to_string()));
    let params = extract_all_params(&method.sig.inputs)?;

    if let Some(execute) = attr.execute {
        if method.sig.receiver().is_some() {
            return Err(Error::new_spanned(
                &method.sig,
                "a deferred op's enqueue fn runs on the caller's thread and cannot take `self`",
            ));
        }
        let Some(timeout) = attr.timeout else {
            return Err(Error::new_spanned(
                &method.sig,
                "a deferred op must declare `timeout = <reason expr>`",
            ));
        };
        let (key_ty, reason_ty) = extract_start_types(&method.sig.output)?;
        Ok(OpMethod {
            method_name,
            variant_name,
            params,
            reason_ty,
            kind: OpKind::Deferred(Box::new(DeferredOp {
                execute,
                timeout,
                key_ty,
            })),
        })
    } else {
        if attr.timeout.is_some() {
            return Err(Error::new_spanned(
                &method.sig,
                "`timeout` applies only to deferred ops, declared with `execute = <method>`",
            ));
        }
        if method.sig.receiver().is_none() {
            return Err(Error::new_spanned(
                &method.sig,
                "an immediate op handler must take `&mut self`",
            ));
        }
        let reason_ty = extract_result_reason(&method.sig.output)?;
        Ok(OpMethod {
            method_name,
            variant_name,
            params,
            reason_ty,
            kind: OpKind::Immediate,
        })
    }
}

/// Extracts `(K, R)` from an enqueue fn's `-> Start<K, R>` return type.
fn extract_start_types(output: &syn::ReturnType) -> Result<(Type, Type)> {
    let err = || {
        Error::new_spanned(
            output,
            "a deferred op's enqueue fn must return `Start<Key, Reason>`",
        )
    };
    let syn::ReturnType::Type(_, ty) = output else {
        return Err(err());
    };
    let Type::Path(path) = &**ty else {
        return Err(err());
    };
    let segment = path.path.segments.last().ok_or_else(err)?;
    if segment.ident != "Start" {
        return Err(err());
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(err());
    };
    let types: Vec<&Type> = args
        .args
        .iter()
        .filter_map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect();
    match types.as_slice() {
        [key_ty, reason_ty] => Ok(((*key_ty).clone(), (*reason_ty).clone())),
        _ => Err(err()),
    }
}

/// Extracts `R` from an immediate op handler's `-> Result<(), R>` return
/// type.
fn extract_result_reason(output: &syn::ReturnType) -> Result<Type> {
    let err = || {
        Error::new_spanned(
            output,
            "an immediate op handler must return `Result<(), Reason>`",
        )
    };
    let syn::ReturnType::Type(_, ty) = output else {
        return Err(err());
    };
    let Type::Path(path) = &**ty else {
        return Err(err());
    };
    let segment = path.path.segments.last().ok_or_else(err)?;
    if segment.ident != "Result" {
        return Err(err());
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(err());
    };
    let types: Vec<&Type> = args
        .args
        .iter()
        .filter_map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect();
    match types.as_slice() {
        [ok_ty, reason_ty] => {
            let is_unit = matches!(ok_ty, Type::Tuple(tuple) if tuple.elems.is_empty());
            if !is_unit {
                return Err(err());
            }
            Ok((*reason_ty).clone())
        }
        _ => Err(err()),
    }
}

/// Derives the subscription method name from the event type, MessageSent
/// becoming on_message_sent.
fn derive_subscription_method(event_type: &Type) -> Result<Ident> {
    let type_name = match event_type {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .ok_or_else(|| {
                Error::new_spanned(
                    event_type,
                    "event handler parameter must be a named event type",
                )
            })?,
        _ => {
            return Err(Error::new_spanned(
                event_type,
                "event handler parameter must be a named event type",
            ));
        }
    };

    let snake_name = to_snake_case(&type_name);
    Ok(format_ident!("on_{}", snake_name))
}

// ============================================================================
// Code Generation
// ============================================================================

fn generate_command_infra(
    struct_name: &Ident,
    command_enum_name: &Ident,
    handle_name: &Ident,
    commands: &[CommandMethod],
    ops: &[OpMethod],
) -> (TokenStream, TokenStream, TokenStream, TokenStream) {
    let cmd_count = commands.len() + ops.len();

    let mut enum_variants: Vec<TokenStream> = commands
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
    enum_variants.extend(ops.iter().map(|op| {
        let variant = &op.variant_name;
        let reason_ty = &op.reason_ty;
        match &op.kind {
            OpKind::Immediate => {
                let types: Vec<_> = op.params.iter().map(|(_, ty)| ty).collect();
                quote! { #variant(#(#types,)* rillet::op::Resolver<#reason_ty>), }
            }
            OpKind::Deferred(deferred) => {
                let key_ty = &deferred.key_ty;
                quote! { #variant(#key_ty, rillet::op::Resolver<#reason_ty>), }
            }
        }
    }));

    let mut index_arms: Vec<TokenStream> = commands
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
    index_arms.extend(ops.iter().enumerate().map(|(i, op)| {
        let idx = commands.len() + i;
        let variant = &op.variant_name;
        quote! { Self::#variant(..) => #idx, }
    }));

    let names: Vec<String> = commands
        .iter()
        .map(|cmd| cmd.method_name.to_string())
        .chain(ops.iter().map(|op| op.method_name.to_string()))
        .collect();

    let index_body = if commands.is_empty() && ops.is_empty() {
        quote! { unreachable!("empty command enum cannot be instantiated") }
    } else {
        quote! {
            match self {
                #(#index_arms)*
            }
        }
    };

    let command_enum = quote! {
        #[allow(clippy::enum_variant_names, clippy::module_name_repetitions)]
        enum #command_enum_name {
            #(#enum_variants)*
        }

        impl #command_enum_name {
            /// Array of command names for metrics.
            const NAMES: [&'static str; #cmd_count] = [#(#names),*];

            /// Returns the index of this command variant.
            #[inline]
            fn index(&self) -> usize {
                #index_body
            }
        }
    };

    let mut match_arms: Vec<TokenStream> = commands
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
    match_arms.extend(ops.iter().map(|op| {
        let variant = &op.variant_name;
        let method = &op.method_name;
        let name_str = op.method_name.to_string();
        match &op.kind {
            OpKind::Immediate => {
                let param_names: Vec<_> = op
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format_ident!("v{}", i))
                    .collect();
                quote! {
                    #command_enum_name::#variant(#(#param_names,)* __resolver) => {
                        match state.#method(#(#param_names),*) {
                            Ok(()) => state
                                .__rillet_ops
                                .queue_conclusion(move || __resolver.succeed()),
                            Err(__reason) => state
                                .__rillet_ops
                                .queue_conclusion(move || __resolver.fail(__reason)),
                        }
                    }
                }
            }
            OpKind::Deferred(deferred) => {
                let DeferredOp {
                    execute,
                    timeout,
                    key_ty,
                } = &**deferred;
                let reason_ty = &op.reason_ty;
                quote! {
                    #command_enum_name::#variant(__key, __resolver) => {
                        state.__rillet_ops.insert::<#key_ty, #reason_ty>(
                            #name_str,
                            __key.clone(),
                            __resolver,
                            || #timeout,
                        );
                        state.#execute(__key)
                    }
                }
            }
        }
    }));

    let execute_impl = quote! {
        impl #command_enum_name {
            fn execute(self, state: &mut #struct_name) {
                match self {
                    #(#match_arms)*
                }
            }
        }
    };

    let mut sender_methods: Vec<TokenStream> = commands
        .iter()
        .enumerate()
        .map(|(idx, cmd)| {
            let method = &cmd.method_name;
            let variant = &cmd.variant_name;
            if cmd.params.is_empty() {
                quote! {
                    pub fn #method(&self) {
                        if rillet::runtime::send_command(
                            &self.cmd_tx,
                            #command_enum_name::#variant,
                            stringify!(#method),
                        ) {
                            self.metrics.inc_enqueued(#idx);
                        }
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
                        if rillet::runtime::send_command(
                            &self.cmd_tx,
                            #command_enum_name::#variant(#(#param_names),*),
                            stringify!(#method),
                        ) {
                            self.metrics.inc_enqueued(#idx);
                        }
                    }
                }
            }
        })
        .collect();
    sender_methods.extend(ops.iter().enumerate().map(|(i, op)| {
        let idx = commands.len() + i;
        let method = &op.method_name;
        let variant = &op.variant_name;
        let reason_ty = &op.reason_ty;
        let param_decls: Vec<TokenStream> = op
            .params
            .iter()
            .map(|(name, ty)| quote! { #name: #ty })
            .collect();
        let param_names: Vec<_> = op.params.iter().map(|(name, _)| name).collect();

        // If the send fails (service gone, queue closed), the dropped
        // resolver concludes the operation as Lost.
        let build_pair_and_command = match &op.kind {
            OpKind::Immediate => quote! {
                let (__op, __resolver) = rillet::op::Op::<#reason_ty>::__rillet_pair(None);
                let __command = #command_enum_name::#variant(#(#param_names,)* __resolver);
            },
            OpKind::Deferred(_) => quote! {
                let (__key, __deadline) =
                    #struct_name::#method(#(#param_names),*).__rillet_parts();
                let (__op, __resolver) = rillet::op::Op::<#reason_ty>::__rillet_pair(__deadline);
                let __command = #command_enum_name::#variant(__key, __resolver);
            },
        };

        quote! {
            /// Enqueues the operation and returns the handle its outcome
            /// lands on.
            pub fn #method(&self, #(#param_decls),*) -> rillet::op::Op<#reason_ty> {
                #build_pair_and_command
                if rillet::runtime::send_command(
                    &self.cmd_tx,
                    __command,
                    stringify!(#method),
                ) {
                    self.metrics.inc_enqueued(#idx);
                }
                __op
            }
        }
    }));

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

/// Generate the keyed conclusion methods for deferred ops:
/// `succeed_<op>` and `fail_<op>` on the service.
fn generate_op_conclusions(struct_name: &Ident, ops: &[OpMethod]) -> TokenStream {
    let methods: Vec<TokenStream> = ops
        .iter()
        .filter_map(|op| {
            let OpKind::Deferred(deferred) = &op.kind else {
                return None;
            };
            let key_ty = &deferred.key_ty;
            let name_str = op.method_name.to_string();
            let reason_ty = &op.reason_ty;
            let succeed_name = format_ident!("succeed_{}", op.method_name);
            let fail_name = format_ident!("fail_{}", op.method_name);
            Some(quote! {
                /// Concludes the operation under this key successfully;
                /// without one, does nothing.
                fn #succeed_name(&self, key: &#key_ty) {
                    self.__rillet_ops.succeed::<#key_ty, #reason_ty>(#name_str, key);
                }

                /// Concludes the operation under this key unsuccessfully;
                /// without one, does nothing.
                fn #fail_name(&self, key: &#key_ty, reason: #reason_ty) {
                    self.__rillet_ops.fail::<#key_ty, #reason_ty>(#name_str, key, reason);
                }
            })
        })
        .collect();

    if methods.is_empty() {
        return quote! {};
    }

    quote! {
        impl #struct_name {
            #(#methods)*
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_spawn_impl(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    has_commands: bool,
    cmd_count: usize,
    has_deferred_ops: bool,
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
            let subscription_method = &handler.subscription_method;
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
            if !inputs_open || !state.read().expect("service state poisoned by a panicked handler").__rillet_has_observers() {
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
                                let mut s = state.write().expect("service state poisoned by a panicked handler");
                                s.#method(event);
                                s.__rillet_publish_view();
                                s.__rillet_ops.flush_conclusions();
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
                    let mut s = state.write().expect("service state poisoned by a panicked handler");
                    s.#method(view);
                    s.__rillet_publish_view();
                    s.__rillet_ops.flush_conclusions();
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

    let has_events = !event_handlers.is_empty() || !watch_handlers.is_empty();

    // The command body and the view republication run under one write lock
    // acquisition, so the published view is the one this command produced.
    let cmd_process_with_metrics = quote! {
        let cmd_idx = cmd.index();
        {
            let mut s = state.write().expect("service state poisoned by a panicked handler");
            cmd.execute(&mut s);
            s.__rillet_publish_view();
            s.__rillet_ops.flush_conclusions();

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
            state
                .read()
                .expect("service state poisoned by a panicked handler")
                .__rillet_ops
                .close();
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

    if has_context_tasks {
        generate_spawn_with_builder(
            struct_name,
            handle_name,
            command_enum_name,
            cmd_count,
            has_deferred_ops,
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
            has_deferred_ops,
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
    has_deferred_ops: bool,
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    let expiry_spawn = if has_deferred_ops {
        quote! {
            {
                let cancel_token = cancel_token.clone();
                let expiry_handle = spawner.spawn(rillet::op::expire_loop(__ops_cell, cancel_token));
                join_handles.push(Box::new(expiry_handle));
            }
        }
    } else {
        quote! {}
    };

    quote! {
        impl #struct_name {
            /// Spawns the service loop; the caller spawns the tasks.
            fn __rillet_spawn_core_with<__S: rillet::runtime::Spawner>(mut self, spawner: &__S) -> (#handle_name, rillet::runtime::Arc<rillet::runtime::TaskSet>) {
                let (cmd_tx, cmd_rx) = rillet::runtime::mpsc::bounded::<#command_enum_name>(#struct_name::__RILLET_COMMAND_CAPACITY);

                let metrics = rillet::runtime::Arc::new(
                    rillet::metrics::CommandMetrics::<#cmd_count>::new()
                );

                self.__rillet_seed_view();
                let __rillet_view = self.__rillet_view_slot_any();
                let __ops_cell = self.__rillet_ops.clone();

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
                    join_handles.push(Box::new(main_loop_handle));
                }

                #expiry_spawn

                (
                    #handle_name {
                        state,
                        cmd_tx,
                        metrics,
                        __rillet_view,
                        __rillet_cancel_token: cancel_token,
                        __rillet_join_handles: join_handles.clone(),
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
            join_handles.push(Box::new(task_handle));
        }
    }
}

/// Generate the plain spawn methods, used when every task is handle-only.
#[allow(clippy::too_many_arguments)]
fn generate_spawn_simple(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    cmd_count: usize,
    has_deferred_ops: bool,
    tasks: &[TaskMethod],
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    let spawn_core = generate_spawn_core(
        struct_name,
        handle_name,
        command_enum_name,
        cmd_count,
        has_deferred_ops,
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
#[allow(clippy::too_many_arguments)]
fn generate_spawn_with_builder(
    struct_name: &Ident,
    handle_name: &Ident,
    command_enum_name: &Ident,
    cmd_count: usize,
    has_deferred_ops: bool,
    tasks: &[TaskMethod],
    event_receiver_setup: &[TokenStream],
    service_loop: &TokenStream,
) -> TokenStream {
    let builder_name = format_ident!("__{struct_name}Builder");

    let context_tasks: Vec<&TaskMethod> = tasks
        .iter()
        .filter(|t| !t.extra_params.is_empty())
        .collect();

    // One const-bool type-state flag per context task; spawn() exists only
    // on the all-armed instantiation.
    let flag_names: Vec<Ident> = context_tasks
        .iter()
        .map(|t| format_ident!("{}", t.method_name.to_string().to_uppercase()))
        .collect();
    let all_true: Vec<TokenStream> = flag_names.iter().map(|_| quote! { true }).collect();

    let builder_fields: Vec<TokenStream> = context_tasks
        .iter()
        .map(|task| {
            let field_name = format_ident!("{}_ctx", task.method_name);
            let types: Vec<_> = task.extra_params.iter().map(|(_, ty)| ty).collect();
            quote! {
                #field_name: Option<(#(#types,)*)>
            }
        })
        .collect();

    // The service's spawn_<task> methods each return the builder with that
    // task armed.
    let service_spawn_methods: Vec<TokenStream> = context_tasks
        .iter()
        .enumerate()
        .map(|(i, task)| {
            let method_name = format_ident!("spawn_{}", task.method_name);
            let field_name = format_ident!("{}_ctx", task.method_name);
            let param_decls: Vec<TokenStream> = task
                .extra_params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();
            let param_names: Vec<_> = task.extra_params.iter().map(|(name, _)| name).collect();

            let ret_args: Vec<TokenStream> = (0..context_tasks.len())
                .map(|j| {
                    if i == j {
                        quote! { true }
                    } else {
                        quote! { false }
                    }
                })
                .collect();

            // The other context fields start as None.
            let other_inits: Vec<TokenStream> = context_tasks
                .iter()
                .filter(|t| t.method_name != task.method_name)
                .map(|t| {
                    let fname = format_ident!("{}_ctx", t.method_name);
                    quote! { #fname: None }
                })
                .collect();

            quote! {
                pub fn #method_name(self, #(#param_decls),*) -> #builder_name<#(#ret_args),*> {
                    #builder_name {
                        inner: self,
                        #field_name: Some((#(#param_names,)*)),
                        #(#other_inits,)*
                    }
                }
            }
        })
        .collect();

    // The builder's spawn_<task> methods chain, each flipping only its own
    // flag; an already-armed task's method does not exist.
    let builder_spawn_methods: Vec<TokenStream> = context_tasks
        .iter()
        .enumerate()
        .map(|(i, task)| {
            let method_name = format_ident!("spawn_{}", task.method_name);
            let field_name = format_ident!("{}_ctx", task.method_name);
            let param_decls: Vec<TokenStream> = task
                .extra_params
                .iter()
                .map(|(name, ty)| quote! { #name: #ty })
                .collect();
            let param_names: Vec<_> = task.extra_params.iter().map(|(name, _)| name).collect();

            let other_flags: Vec<&Ident> = flag_names
                .iter()
                .enumerate()
                .filter_map(|(j, flag)| (i != j).then_some(flag))
                .collect();
            let self_args: Vec<TokenStream> = flag_names
                .iter()
                .enumerate()
                .map(|(j, flag)| {
                    if i == j {
                        quote! { false }
                    } else {
                        quote! { #flag }
                    }
                })
                .collect();
            let ret_args: Vec<TokenStream> = flag_names
                .iter()
                .enumerate()
                .map(|(j, flag)| {
                    if i == j {
                        quote! { true }
                    } else {
                        quote! { #flag }
                    }
                })
                .collect();

            let other_moves: Vec<TokenStream> = context_tasks
                .iter()
                .filter(|t| t.method_name != task.method_name)
                .map(|t| {
                    let fname = format_ident!("{}_ctx", t.method_name);
                    quote! { #fname: self.#fname }
                })
                .collect();

            quote! {
                impl<#(const #other_flags: bool),*> #builder_name<#(#self_args),*> {
                    pub fn #method_name(self, #(#param_decls),*) -> #builder_name<#(#ret_args),*> {
                        #builder_name {
                            inner: self.inner,
                            #field_name: Some((#(#param_names,)*)),
                            #(#other_moves,)*
                        }
                    }
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
                    {
                        let (#(#param_names,)*) = self
                            .#field_name
                            .expect("the builder type state arms every context task");
                        let handle = handle.clone();
                        let cancel_token = cancel_token.clone();
                        let task_handle = spawner.spawn(async move {
                            #struct_name::#method_name(handle, cancel_token, #(#param_names),*).await;
                        });
                        join_handles.push(Box::new(task_handle));
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
        has_deferred_ops,
        event_receiver_setup,
        service_loop,
    );

    quote! {
        /// A spawn builder tracking which context tasks are armed; the
        /// spawn methods exist once every one of them is.
        #[doc(hidden)]
        pub struct #builder_name<#(const #flag_names: bool),*> {
            inner: #struct_name,
            #(#builder_fields,)*
        }

        #spawn_core

        impl #struct_name {
            #(#service_spawn_methods)*
        }

        #(#builder_spawn_methods)*

        impl #builder_name<#(#all_true),*> {
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
                    self.state.read().expect("service state poisoned by a panicked handler").#method_name(#(#param_names),*)
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
                    let mut s = self.state.write().expect("service state poisoned by a panicked handler");
                    let result = s.#method_name(#(#param_names),*);
                    s.__rillet_publish_view();
                    s.__rillet_ops.flush_conclusions();
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

#[cfg(test)]
mod tests {
    use super::parse_method_attrs;
    use syn::parse_quote;

    #[test]
    fn combined_role_conflict_is_rejected() {
        let method: syn::ImplItemFn = parse_quote! {
            #[rillet(command, direct)]
            fn clear(&mut self) {}
        };
        let err = parse_method_attrs(&method)
            .err()
            .expect("conflict must be rejected");
        assert!(err.to_string().contains("`command`, `direct`"));
    }

    #[test]
    fn stacked_role_conflict_is_rejected() {
        let method: syn::ImplItemFn = parse_quote! {
            #[rillet(task)]
            #[rillet(from = bird)]
            fn feed(&mut self) {}
        };
        let err = parse_method_attrs(&method)
            .err()
            .expect("conflict must be rejected");
        assert!(err.to_string().contains("only one role"));
    }

    #[test]
    fn single_role_is_accepted() {
        let method: syn::ImplItemFn = parse_quote! {
            #[rillet(command)]
            fn clear(&mut self) {}
        };
        assert!(parse_method_attrs(&method).is_ok());
    }
}
