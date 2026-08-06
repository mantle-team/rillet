//! Procedural macros for the rillet actor framework.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod case;
mod cheap_clone;
mod event;
mod handlers;
mod service;

/// Derives the CheapClone marker for a type whose fields are all CheapClone.
///
/// A field whose type does not implement `CheapClone` fails to compile at
/// that field's span.
#[proc_macro_derive(CheapClone)]
pub fn derive_cheap_clone(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    cheap_clone::expand(input)
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Derives the Event marker for an event type.
///
/// The type must be `Clone`.
#[proc_macro_derive(Event)]
pub fn derive_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    event::expand(input)
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Marks an impl block whose annotated methods are a service's handlers.
///
/// `#[rillet(command)]` methods gain a queueing method on the handle and run
/// serially on the service loop. `#[rillet(from = field)]` methods run for
/// each event of the parameter's type emitted by that field.
/// `#[rillet(watch = field)]` methods run for each view published by that
/// field. `#[rillet(direct)]` and `#[rillet(direct_mut)]` methods run on the
/// caller's thread under the service's lock. `#[rillet(task)]` methods are
/// spawned alongside the service.
///
/// # Example
///
/// ```rust,ignore
/// #[rillet::handlers]
/// impl Chat {
///     // Queued from the handle, run on the service loop.
///     #[rillet(command)]
///     fn send(&mut self, msg: Message) {
///         self.emit_message_sent(MessageSent { message: msg.clone() });
///         self.messages.push(msg);
///     }
/// }
///
/// #[rillet::handlers]
/// impl Analytics {
///     // Runs for each MessageSent emitted by the `chat` field.
///     #[rillet(from = chat)]
///     fn on_message(&mut self, event: MessageSent) {
///         self.count += 1;
///     }
///
///     // Runs for each view published by the `peers` field.
///     #[rillet(watch = peers)]
///     fn on_peers_view(&mut self, view: Arc<PeersView>) {
///         self.peer_count = view.peers.len();
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn handlers(attr: TokenStream, item: TokenStream) -> TokenStream {
    handlers::expand(attr.into(), item.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Turns a struct into a service.
///
/// The macro:
/// - Injects an emitter field for services that emit events
/// - Generates the handle struct
/// - Generates a `new()` constructor (fields with `#[rillet(default)]` are excluded)
/// - Generates getter methods for `#[rillet(get)]` fields
/// - Generates `emit_<event>()` methods for each declared event
/// - Generates subscription methods on the handle
///
/// # View publication
///
/// `#[rillet::service(view = MyView)]` declares that the service publishes a
/// view. The service must provide `fn view(&self) -> MyView`, and `MyView`
/// must be `CheapClone + PartialEq + Send + Sync + 'static`. The generated
/// write path recomputes the view after every mutation and publishes it when
/// it changed, waking watchers. The handle gains wait-free `view()` and
/// `watch_view()` methods, and a `{Name}ViewHandle` type is generated
/// exposing only those and event subscriptions.
///
/// # Capacities
///
/// `command_capacity = N` sizes the command queue and `event_capacity = N`
/// sizes each event channel; both default to 256. A full command queue or
/// event channel panics rather than dropping.
///
/// # Field Attributes
///
/// - `#[rillet(get)]`: generates a getter method on the handle that clones the field
/// - `#[rillet(default)]`: excludes the field from `new()`, initializes it with `Default::default()`
/// - `#[rillet(default = <expr>)]`: excludes the field from `new()`, initializes it with the expression
///
/// Field attributes can be combined: `#[rillet(get, default)]`
///
/// # Example
///
/// ```rust,ignore
/// #[rillet::service]
/// #[rillet(emits = [MessageSent])]
/// struct Chat {
///     #[rillet(get, default)]
///     messages: Vec<Message>,
///
///     #[rillet(default = 100)]
///     max_messages: usize,
/// }
///
/// #[rillet::handlers]
/// impl Chat {
///     #[rillet(command)]
///     fn send(&mut self, msg: Message) {
///         self.emit_message_sent(MessageSent { ... });
///         self.messages.push(msg);
///     }
/// }
///
/// // Both fields default, so new() takes no arguments.
/// let chat = Chat::new().spawn();
/// ```
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    service::expand(attr.into(), item.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}
