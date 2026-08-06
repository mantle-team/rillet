//! Typed broadcast events between services.
//!
//! A service declares the event types it emits; each type gets its own
//! bounded broadcast channel, and a subscriber receives every event
//! published after it subscribed.

use async_broadcast::{Receiver, Sender, TrySendError};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Marker trait for event types.
///
/// Implemented automatically by `#[derive(Event)]`.
pub trait Event: Clone + Send + 'static {}

/// A receiver for events of one type from a service.
///
/// Each receiver holds its own queue of events it has not yet taken.
pub struct EventReceiver<T>(Receiver<T>);

impl<T: Clone> EventReceiver<T> {
    /// Creates a new EventReceiver from a broadcast receiver.
    pub fn new(rx: Receiver<T>) -> Self {
        Self(rx)
    }

    /// Returns the next event without waiting, or `None` if none is queued.
    pub fn try_recv(&mut self) -> Option<T> {
        self.0.try_recv().ok()
    }

    /// Blocks the thread until the next event, or `None` once the channel
    /// closes.
    pub fn recv(&mut self) -> Option<T> {
        futures_lite::future::block_on(self.next())
    }

    /// Waits for the next event, or `None` once the channel closes.
    pub async fn next(&mut self) -> Option<T> {
        self.0.recv().await.ok()
    }

    /// Returns the channel's capacity in events.
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    /// Returns the number of events waiting in this receiver's queue.
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

/// A publisher of a service's events, holding one broadcast channel per
/// declared event type.
#[derive(Clone)]
pub struct Emitter {
    senders: Arc<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
    /// Published count per event type.
    counters: Arc<HashMap<TypeId, AtomicU64>>,
    /// Keep inactive receivers alive to prevent channels from closing.
    /// async_broadcast closes the channel when all receivers (including inactive) are dropped.
    _inactive_receivers: Arc<Vec<Box<dyn Any + Send + Sync>>>,
}

impl Emitter {
    /// Creates a new empty emitter.
    pub fn new() -> Self {
        Self {
            senders: Arc::new(HashMap::new()),
            counters: Arc::new(HashMap::new()),
            _inactive_receivers: Arc::new(Vec::new()),
        }
    }

    /// Creates an emitter with pre-registered senders.
    #[doc(hidden)]
    pub fn with_senders(
        senders: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
        counters: HashMap<TypeId, AtomicU64>,
        inactive_receivers: Vec<Box<dyn Any + Send + Sync>>,
    ) -> Self {
        Self {
            senders: Arc::new(senders),
            counters: Arc::new(counters),
            _inactive_receivers: Arc::new(inactive_receivers),
        }
    }

    /// Emits an event to all subscribers.
    ///
    /// Panics when any subscriber's queue is full; an emit with no
    /// subscribers is a no-op.
    pub fn emit<E: Event>(&self, event: E) {
        if let Some(sender) = self.senders.get(&TypeId::of::<E>())
            && let Some(tx) = sender.downcast_ref::<Sender<E>>()
        {
            match tx.try_broadcast(event) {
                Ok(_) => {}
                // No active subscribers; the event has no audience.
                Err(TrySendError::Inactive(_)) | Err(TrySendError::Closed(_)) => {}
                Err(TrySendError::Full(_)) => {
                    panic!("event queue full: {}", std::any::type_name::<E>());
                }
            }
            if let Some(counter) = self.counters.get(&TypeId::of::<E>()) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns a receiver for all future events of this type.
    ///
    /// Panics if the event type was not declared on this emitter.
    pub fn subscribe<E: Event>(&self) -> Receiver<E> {
        self.senders
            .get(&TypeId::of::<E>())
            .and_then(|sender| sender.downcast_ref::<Sender<E>>())
            .expect("event type not registered with this emitter")
            .new_receiver()
    }

    /// Returns the number of events published for this event type.
    pub fn published<E: Event>(&self) -> u64 {
        self.counters
            .get(&TypeId::of::<E>())
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Returns the current number of subscribers for this event type.
    pub fn subscriber_count<E: Event>(&self) -> usize {
        self.senders
            .get(&TypeId::of::<E>())
            .and_then(|sender| sender.downcast_ref::<Sender<E>>())
            .map(|tx| tx.receiver_count())
            .unwrap_or(0)
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to build an emitter with typed senders.
#[doc(hidden)]
pub struct EmitterBuilder {
    senders: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    counters: HashMap<TypeId, AtomicU64>,
    // Keep inactive receivers alive to prevent channel from closing
    _inactive_receivers: Vec<Box<dyn Any + Send + Sync>>,
}

impl Default for EmitterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EmitterBuilder {
    pub fn new() -> Self {
        Self {
            senders: HashMap::new(),
            counters: HashMap::new(),
            _inactive_receivers: Vec::new(),
        }
    }

    /// Adds a broadcast sender for an event type, returns the sender for subscription.
    pub fn add_event<E: Event>(&mut self, capacity: usize) -> Sender<E> {
        let (tx, rx) = async_broadcast::broadcast(capacity);
        self.senders.insert(TypeId::of::<E>(), Box::new(tx.clone()));
        self.counters.insert(TypeId::of::<E>(), AtomicU64::new(0));
        // Keep an inactive receiver alive to prevent the channel from
        // closing. It must be deactivated: an active receiver that is
        // never drained fills after `capacity` events and blocks every
        // subsequent broadcast on this channel.
        self._inactive_receivers.push(Box::new(rx.deactivate()));
        tx
    }

    pub fn build(self) -> Emitter {
        Emitter::with_senders(self.senders, self.counters, self._inactive_receivers)
    }
}

#[cfg(test)]
mod tests {
    use super::{EmitterBuilder, Event};

    #[derive(Clone, Debug, PartialEq)]
    struct Ping(u32);
    impl Event for Ping {}

    #[test]
    fn emits_beyond_capacity_reach_late_subscribers() {
        let mut builder = EmitterBuilder::new();
        builder.add_event::<Ping>(4);
        let emitter = builder.build();

        // Far more events than the channel holds; with no draining
        // subscriber these must not wedge the channel.
        for i in 0..32 {
            emitter.emit(Ping(i));
        }

        let mut rx = emitter.subscribe::<Ping>();
        emitter.emit(Ping(99));
        assert_eq!(rx.try_recv(), Ok(Ping(99)));
    }

    #[test]
    #[should_panic(expected = "event queue full")]
    fn panics_when_a_subscriber_falls_behind() {
        let mut builder = EmitterBuilder::new();
        builder.add_event::<Ping>(2);
        let emitter = builder.build();

        let _rx = emitter.subscribe::<Ping>();
        emitter.emit(Ping(0));
        emitter.emit(Ping(1));
        emitter.emit(Ping(2));
    }
}
