//! View publication for services.
//!
//! A view is an immutable snapshot of a service's state, published after
//! every mutation and readable without taking any lock. A service opts in
//! with `#[rillet::service(view = MyView)]` and provides
//! `fn view(&self) -> MyView`; the generated write path recomputes the view
//! while still holding the state lock, so a view is always coherent with the
//! state that produced it, and publishes it only when it differs from the
//! previous one. View types must be [`CheapClone`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use event_listener::Event;

#[cfg(feature = "im")]
pub use im;
#[cfg(feature = "smol-str")]
pub use smol_str::SmolStr;

/// Marker for types that clone by copying pointers, bumping reference
/// counts, or sharing structure rather than duplicating their contents.
///
/// `#[derive(CheapClone)]` implements it for structs whose fields all
/// implement it. A hand-written impl carries no such check.
pub trait CheapClone: Clone {}

macro_rules! impl_cheap_clone {
    ($($ty:ty),* $(,)?) => {
        $(impl CheapClone for $ty {})*
    };
}

impl_cheap_clone!(
    (),
    bool,
    char,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    std::time::Duration,
    std::time::Instant,
);

impl<T: ?Sized> CheapClone for Arc<T> {}
impl<T: ?Sized> CheapClone for &T {}
impl<T: CheapClone> CheapClone for Option<T> {}

macro_rules! impl_cheap_clone_tuple {
    ($($name:ident),+) => {
        impl<$($name: CheapClone),+> CheapClone for ($($name,)+) {}
    };
}

impl_cheap_clone_tuple!(A);
impl_cheap_clone_tuple!(A, B);
impl_cheap_clone_tuple!(A, B, C);
impl_cheap_clone_tuple!(A, B, C, D);

#[cfg(feature = "smol-str")]
impl CheapClone for SmolStr {}

#[cfg(feature = "im")]
mod im_impls {
    use super::CheapClone;

    impl<A: Clone> CheapClone for im::Vector<A> {}
    impl<K: Clone + std::hash::Hash + Eq, V: Clone, S: std::hash::BuildHasher> CheapClone
        for im::HashMap<K, V, S>
    {
    }
    impl<A: Clone + std::hash::Hash + Eq, S: std::hash::BuildHasher> CheapClone for im::HashSet<A, S> {}
    impl<K: Clone + Ord, V: Clone> CheapClone for im::OrdMap<K, V> {}
    impl<A: Clone + Ord> CheapClone for im::OrdSet<A> {}
}

/// A slot holding a service's latest published view.
///
/// Loads are wait-free.
pub struct ViewSlot<V> {
    shared: Arc<Shared<V>>,
}

struct Shared<V> {
    current: ArcSwap<V>,
    version: AtomicU64,
    wake: Event,
    watchers: AtomicUsize,
}

impl<V> ViewSlot<V>
where
    V: CheapClone + PartialEq + Send + Sync + 'static,
{
    /// Creates a slot holding an initial view.
    pub fn new(initial: V) -> Self {
        Self {
            shared: Arc::new(Shared {
                current: ArcSwap::from_pointee(initial),
                version: AtomicU64::new(0),
                wake: Event::new(),
                watchers: AtomicUsize::new(0),
            }),
        }
    }

    /// Returns the latest published view.
    pub fn load(&self) -> Arc<V> {
        self.shared.current.load_full()
    }

    /// Stores a view if it differs from the current one, waking the
    /// watchers; returns whether it stored.
    pub fn publish(&self, next: V) -> bool {
        if **self.shared.current.load() == next {
            return false;
        }
        self.shared.current.store(Arc::new(next));
        self.shared.version.fetch_add(1, Ordering::Release);
        self.shared.wake.notify(usize::MAX);
        true
    }

    /// Returns a watcher that has already seen the current view.
    pub fn watch(&self) -> ViewWatcher<V> {
        self.shared.watchers.fetch_add(1, Ordering::Relaxed);
        ViewWatcher {
            shared: self.shared.clone(),
            seen: self.shared.version.load(Ordering::Acquire),
        }
    }

    /// Returns the current number of watchers.
    pub fn watcher_count(&self) -> usize {
        self.shared.watchers.load(Ordering::Relaxed)
    }
}

impl<V> Clone for ViewSlot<V> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

/// A subscription to a service's view publications.
///
/// A watcher records the version it last returned, not the views
/// themselves; each wake reads the newest view, and any it missed are
/// skipped rather than queued.
pub struct ViewWatcher<V> {
    shared: Arc<Shared<V>>,
    seen: u64,
}

impl<V> Drop for ViewWatcher<V> {
    fn drop(&mut self) {
        self.shared.watchers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<V> ViewWatcher<V>
where
    V: CheapClone + PartialEq + Send + Sync + 'static,
{
    /// Waits until a view newer than the last one seen is published, and
    /// returns it.
    ///
    /// Never completes if nothing is published again.
    pub async fn changed(&mut self) -> Arc<V> {
        loop {
            let listener = self.shared.wake.listen();
            let version = self.shared.version.load(Ordering::Acquire);
            if version != self.seen {
                self.seen = version;
                return self.shared.current.load_full();
            }
            listener.await;
        }
    }

    /// Returns the current view if it is newer than the last one seen.
    pub fn try_changed(&mut self) -> Option<Arc<V>> {
        let version = self.shared.version.load(Ordering::Acquire);
        if version != self.seen {
            self.seen = version;
            Some(self.shared.current.load_full())
        } else {
            None
        }
    }

    /// Returns the latest published view without marking it seen.
    pub fn current(&self) -> Arc<V> {
        self.shared.current.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_initial() {
        let slot = ViewSlot::new(7u32);
        assert_eq!(*slot.load(), 7);
    }

    #[test]
    fn publish_stores_changed_value() {
        let slot = ViewSlot::new(7u32);
        assert!(slot.publish(8));
        assert_eq!(*slot.load(), 8);
    }

    #[test]
    fn publish_dedups_unchanged_value() {
        let slot = ViewSlot::new(7u32);
        let before = slot.load();
        assert!(!slot.publish(7));
        assert!(Arc::ptr_eq(&before, &slot.load()));
    }

    #[test]
    fn watcher_sees_only_future_publishes() {
        let slot = ViewSlot::new(7u32);
        slot.publish(8);
        let mut watcher = slot.watch();
        assert!(watcher.try_changed().is_none());
        slot.publish(9);
        assert_eq!(watcher.try_changed().as_deref(), Some(&9));
    }

    #[test]
    fn lagging_watcher_wakes_to_the_latest_view_only() {
        let slot = ViewSlot::new(0u32);
        let mut watcher = slot.watch();
        for value in 1..=100 {
            slot.publish(value);
        }
        assert_eq!(watcher.try_changed().as_deref(), Some(&100));
        assert!(watcher.try_changed().is_none());
    }

    #[test]
    fn changed_wakes_across_threads() {
        let slot = ViewSlot::new(0u32);
        let mut watcher = slot.watch();
        let handle = std::thread::spawn(move || futures_lite::future::block_on(watcher.changed()));
        std::thread::sleep(std::time::Duration::from_millis(20));
        slot.publish(5);
        assert_eq!(*handle.join().unwrap(), 5);
    }
}
