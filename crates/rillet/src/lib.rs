//! An actor framework with lock-free reads.
//!
//! A service owns its state behind an `Arc<RwLock<T>>`. Mutations are
//! serialized through an async command channel, events flow between services
//! over broadcast channels, and a service may publish a [view](crate::view):
//! an immutable snapshot of its state that readers load without taking any
//! lock.

mod cancellation;
mod spawner;

pub mod event;
pub mod metrics;
pub mod view;

#[doc(hidden)]
pub mod runtime;

pub use rillet_macros::CheapClone;
pub use rillet_macros::Event;

pub use rillet_macros::handlers;
pub use rillet_macros::service;

pub use cancellation::CancellationToken;
pub use event::EventReceiver;
pub use spawner::{SmolSpawner, Spawner, TaskHandle};
