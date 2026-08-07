//! Operations: commands whose outcome arrives later.
//!
//! An op handler's generated handle method returns an [`Op`] the moment the
//! command is enqueued. The service concludes the operation once its outcome
//! is known: immediately, by returning a `Result` from the handler, or
//! later, by calling the generated `succeed_<op>` / `fail_<op>` methods when
//! the outcome arrives. An operation registered with a deadline and not
//! concluded in time fails with the op's declared timeout reason.
//!
//! The caller reads or waits on the [`Op`] to follow the lifecycle.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwap;
use event_listener::{Event, EventListener};

use crate::cancellation::CancellationToken;

/// The lifecycle of an operation.
#[derive(Clone, Debug, PartialEq)]
pub enum OpState<R> {
    /// Started and awaiting its outcome.
    Pending {
        since: Instant,
        /// When the operation expires, if it can time out.
        deadline: Option<Instant>,
    },
    /// Concluded successfully.
    Done { at: Instant },
    /// Concluded unsuccessfully.
    Failed { reason: R, at: Instant },
    /// Concluded without an outcome; none will ever arrive.
    Lost { at: Instant },
}

impl<R> OpState<R> {
    /// Whether the operation is awaiting its outcome.
    pub fn is_pending(&self) -> bool {
        matches!(self, OpState::Pending { .. })
    }

    /// Whether the operation has concluded.
    pub fn is_terminal(&self) -> bool {
        !self.is_pending()
    }

    /// The failure reason, if the operation concluded unsuccessfully.
    pub fn failure(&self) -> Option<&R> {
        match self {
            OpState::Failed { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

struct Shared<R> {
    state: ArcSwap<OpState<R>>,
    wake: Event,
}

impl<R> Shared<R> {
    fn store(&self, next: OpState<R>) {
        self.state.store(Arc::new(next));
        self.wake.notify(usize::MAX);
    }
}

/// A caller's handle to one operation.
///
/// Reading the state never blocks. A consumer can wait for a change with
/// [`listen`](Op::listen) and read the state again when it fires, or wait
/// for the outcome with [`concluded`](Op::concluded).
pub struct Op<R> {
    shared: Arc<Shared<R>>,
}

impl<R> Op<R> {
    #[doc(hidden)]
    pub fn __rillet_pair() -> (Op<R>, Resolver<R>) {
        let shared = Arc::new(Shared {
            state: ArcSwap::from_pointee(OpState::Pending {
                since: Instant::now(),
                deadline: None,
            }),
            wake: Event::new(),
        });
        (
            Op {
                shared: shared.clone(),
            },
            Resolver {
                shared,
                resolved: false,
            },
        )
    }

    /// The current state.
    pub fn state(&self) -> Arc<OpState<R>> {
        self.shared.state.load_full()
    }

    /// Returns a listener that resolves at the next state change.
    ///
    /// Take the listener before reading the state, so a change landing
    /// between the two is not missed.
    pub fn listen(&self) -> EventListener {
        self.shared.wake.listen()
    }

    /// Waits until the operation concludes and returns its final state.
    pub async fn concluded(&self) -> Arc<OpState<R>> {
        loop {
            let listener = self.listen();
            let state = self.state();
            if state.is_terminal() {
                return state;
            }
            listener.await;
        }
    }
}

impl<R> Clone for Op<R> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

/// Concludes one operation with its outcome.
///
/// Concluding consumes the resolver, so an operation concludes exactly
/// once. Dropping a resolver while its operation is pending concludes it
/// as [`Lost`](OpState::Lost).
#[doc(hidden)]
pub struct Resolver<R> {
    shared: Arc<Shared<R>>,
    resolved: bool,
}

impl<R> Resolver<R> {
    /// Concludes the operation successfully.
    pub fn succeed(mut self) {
        self.resolved = true;
        self.shared.store(OpState::Done { at: Instant::now() });
    }

    /// Concludes the operation unsuccessfully.
    pub fn fail(mut self, reason: R) {
        self.resolved = true;
        self.shared.store(OpState::Failed {
            reason,
            at: Instant::now(),
        });
    }

    fn set_deadline(&self, deadline: Option<Instant>) {
        if let OpState::Pending { since, .. } = **self.shared.state.load() {
            self.shared.store(OpState::Pending { since, deadline });
        }
    }
}

impl<R> Drop for Resolver<R> {
    fn drop(&mut self) {
        if !self.resolved {
            self.shared.store(OpState::Lost { at: Instant::now() });
        }
    }
}

/// The enqueue-time description of a deferred operation: its key and
/// optional deadline, produced by the op handler's enqueue fn.
pub struct Start<K, R> {
    key: K,
    deadline: Option<Instant>,
    _reason: PhantomData<R>,
}

impl<K, R> Start<K, R> {
    /// Describes an operation under the given key, with no deadline.
    pub fn new(key: K) -> Self {
        Self {
            key,
            deadline: None,
            _reason: PhantomData,
        }
    }

    /// Sets the instant at which the operation expires.
    pub fn deadline(mut self, at: Instant) -> Self {
        self.deadline = Some(at);
        self
    }

    #[doc(hidden)]
    pub fn __rillet_parts(self) -> (K, Option<Instant>) {
        (self.key, self.deadline)
    }
}

/// One service's deferred operations, one registry per op method.
///
/// Injected into every service; the generated op plumbing is its only
/// caller.
#[doc(hidden)]
pub struct OpsCell {
    inner: Arc<CellInner>,
}

struct CellInner {
    registries: Mutex<HashMap<&'static str, Arc<dyn DeadlineSource>>>,
    wake: Event,
}

impl OpsCell {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CellInner {
                registries: Mutex::new(HashMap::new()),
                wake: Event::new(),
            }),
        }
    }

    /// Parks a resolver under a key, stamping the deadline onto its
    /// operation. A resolver already parked under the key is displaced and
    /// its operation concludes as `Lost`.
    pub fn insert<K, R>(
        &self,
        op: &'static str,
        key: K,
        resolver: Resolver<R>,
        deadline: Option<Instant>,
        timeout_reason: impl FnOnce() -> R,
    ) where
        K: Ord + Clone + Send + 'static,
        R: Clone + Send + Sync + 'static,
    {
        let registry = self.registry_or_init::<K, R>(op, timeout_reason);
        resolver.set_deadline(deadline);
        registry.insert(key, resolver, deadline);
        self.inner.wake.notify(usize::MAX);
    }

    /// Concludes the operation under a key successfully; without one, does
    /// nothing.
    pub fn succeed<K, R>(&self, op: &'static str, key: &K)
    where
        K: Ord + Clone + Send + 'static,
        R: Clone + Send + Sync + 'static,
    {
        if let Some(resolver) = self.take::<K, R>(op, key) {
            resolver.succeed();
        }
    }

    /// Concludes the operation under a key unsuccessfully; without one,
    /// does nothing.
    pub fn fail<K, R>(&self, op: &'static str, key: &K, reason: R)
    where
        K: Ord + Clone + Send + 'static,
        R: Clone + Send + Sync + 'static,
    {
        if let Some(resolver) = self.take::<K, R>(op, key) {
            resolver.fail(reason);
        }
    }

    fn take<K, R>(&self, op: &'static str, key: &K) -> Option<Resolver<R>>
    where
        K: Ord + Clone + Send + 'static,
        R: Clone + Send + Sync + 'static,
    {
        let registry = {
            let registries = self
                .inner
                .registries
                .lock()
                .expect("ops registries poisoned");
            registries.get(op)?.clone()
        };
        let registry = registry
            .as_any()
            .downcast_ref::<Registry<K, R>>()
            .expect("op registered under one key and reason type");
        registry.take(key)
    }

    fn registry_or_init<K, R>(
        &self,
        op: &'static str,
        timeout_reason: impl FnOnce() -> R,
    ) -> Arc<Registry<K, R>>
    where
        K: Ord + Clone + Send + 'static,
        R: Clone + Send + Sync + 'static,
    {
        let mut registries = self
            .inner
            .registries
            .lock()
            .expect("ops registries poisoned");
        let registry = registries
            .entry(op)
            .or_insert_with(|| Arc::new(Registry::<K, R>::new(timeout_reason())))
            .clone();
        drop(registries);
        registry
            .as_arc_any()
            .downcast::<Registry<K, R>>()
            .expect("op registered under one key and reason type")
    }

    fn sources(&self) -> Vec<Arc<dyn DeadlineSource>> {
        self.inner
            .registries
            .lock()
            .expect("ops registries poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.sources()
            .iter()
            .filter_map(|source| source.next_deadline())
            .min()
    }

    fn expire(&self, now: Instant) {
        for source in self.sources() {
            source.expire(now);
        }
    }

    fn listen(&self) -> EventListener {
        self.inner.wake.listen()
    }
}

impl Default for OpsCell {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for OpsCell {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Fails overdue operations until cancelled.
///
/// Spawned once per service that declares op handlers.
#[doc(hidden)]
pub async fn expire_loop(cell: OpsCell, cancel: CancellationToken) {
    loop {
        let listener = cell.listen();
        if cancel.is_cancelled() {
            return;
        }
        let wait = async {
            match cell.next_deadline() {
                Some(at) => {
                    smol::Timer::at(at).await;
                }
                None => std::future::pending().await,
            }
        };
        let woken = async {
            listener.await;
        };
        futures_lite::future::or(
            async {
                cancel.cancelled().await;
            },
            futures_lite::future::or(wait, woken),
        )
        .await;
        if cancel.is_cancelled() {
            return;
        }
        cell.expire(Instant::now());
    }
}

/// The typed registry behind one op method's deferred operations.
struct Registry<K, R> {
    entries: Mutex<Entries<K, R>>,
    timeout_reason: R,
}

struct Entries<K, R> {
    by_key: BTreeMap<K, Entry<R>>,
    deadlines: BTreeSet<(Instant, K)>,
}

struct Entry<R> {
    resolver: Resolver<R>,
    deadline: Option<Instant>,
}

impl<K, R> Registry<K, R>
where
    K: Ord + Clone + Send + 'static,
    R: Clone + Send + Sync + 'static,
{
    fn new(timeout_reason: R) -> Self {
        Self {
            entries: Mutex::new(Entries {
                by_key: BTreeMap::new(),
                deadlines: BTreeSet::new(),
            }),
            timeout_reason,
        }
    }

    fn insert(&self, key: K, resolver: Resolver<R>, deadline: Option<Instant>) {
        let mut entries = self.entries.lock().expect("op registry poisoned");
        entries.remove(&key);
        if let Some(deadline) = deadline {
            entries.deadlines.insert((deadline, key.clone()));
        }
        entries.by_key.insert(key, Entry { resolver, deadline });
    }

    fn take(&self, key: &K) -> Option<Resolver<R>> {
        self.entries
            .lock()
            .expect("op registry poisoned")
            .remove(key)
    }
}

impl<K: Ord + Clone, R> Entries<K, R> {
    fn remove(&mut self, key: &K) -> Option<Resolver<R>> {
        let entry = self.by_key.remove(key)?;
        if let Some(deadline) = entry.deadline {
            self.deadlines.remove(&(deadline, key.clone()));
        }
        Some(entry.resolver)
    }
}

trait DeadlineSource: Send + Sync {
    fn next_deadline(&self) -> Option<Instant>;
    fn expire(&self, now: Instant);
    fn as_any(&self) -> &dyn Any;
    fn as_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

impl<K, R> DeadlineSource for Registry<K, R>
where
    K: Ord + Clone + Send + 'static,
    R: Clone + Send + Sync + 'static,
{
    fn next_deadline(&self) -> Option<Instant> {
        self.entries
            .lock()
            .expect("op registry poisoned")
            .deadlines
            .first()
            .map(|(deadline, _)| *deadline)
    }

    fn expire(&self, now: Instant) {
        let mut due = Vec::new();
        {
            let mut entries = self.entries.lock().expect("op registry poisoned");
            while let Some((deadline, key)) = entries.deadlines.first().cloned() {
                if deadline > now {
                    break;
                }
                if let Some(resolver) = entries.remove(&key) {
                    due.push(resolver);
                }
            }
        }
        for resolver in due {
            resolver.fail(self.timeout_reason.clone());
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    enum Reason {
        Declined,
        TimedOut,
    }

    #[test]
    fn new_op_is_pending() {
        let (op, _resolver) = Op::<Reason>::__rillet_pair();
        assert!(op.state().is_pending());
    }

    #[test]
    fn succeed_concludes_the_operation() {
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        resolver.succeed();
        assert!(matches!(*op.state(), OpState::Done { .. }));
    }

    #[test]
    fn fail_records_the_reason() {
        let (op, resolver) = Op::__rillet_pair();
        resolver.fail(Reason::Declined);
        assert_eq!(op.state().failure(), Some(&Reason::Declined));
    }

    #[test]
    fn dropped_resolver_concludes_as_lost() {
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        drop(resolver);
        assert!(matches!(*op.state(), OpState::Lost { .. }));
    }

    #[test]
    fn resolution_is_visible_before_the_wake() {
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        let listener = op.listen();
        resolver.succeed();
        futures_lite::future::block_on(listener);
        assert!(op.state().is_terminal());
    }

    #[test]
    fn concluded_returns_the_final_state() {
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        let waiter = op.clone();
        let handle = std::thread::spawn(move || futures_lite::future::block_on(waiter.concluded()));
        std::thread::sleep(Duration::from_millis(20));
        resolver.fail(Reason::Declined);
        assert_eq!(handle.join().unwrap().failure(), Some(&Reason::Declined));
    }

    #[test]
    fn cell_concludes_by_key() {
        let cell = OpsCell::new();
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert("send", 1u32, resolver, None, || Reason::TimedOut);

        cell.succeed::<u32, Reason>("send", &1);
        assert!(matches!(*op.state(), OpState::Done { .. }));

        // A late outcome finds nothing to conclude.
        cell.fail::<u32, Reason>("send", &1, Reason::Declined);
        assert!(matches!(*op.state(), OpState::Done { .. }));
    }

    #[test]
    fn insert_stamps_the_deadline_onto_the_operation() {
        let cell = OpsCell::new();
        let (op, resolver) = Op::<Reason>::__rillet_pair();
        let deadline = Instant::now() + Duration::from_secs(60);
        cell.insert("send", 1u32, resolver, Some(deadline), || Reason::TimedOut);
        assert!(
            matches!(*op.state(), OpState::Pending { deadline: Some(at), .. } if at == deadline)
        );
    }

    #[test]
    fn insert_displaces_a_parked_operation() {
        let cell = OpsCell::new();
        let (first, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert("send", 1u32, resolver, None, || Reason::TimedOut);
        let (second, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert("send", 1u32, resolver, None, || Reason::TimedOut);

        assert!(matches!(*first.state(), OpState::Lost { .. }));
        assert!(second.state().is_pending());
    }

    #[test]
    fn expire_fails_only_operations_past_their_deadline() {
        let cell = OpsCell::new();
        let now = Instant::now();
        let (due, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert("send", 1u32, resolver, Some(now), || Reason::TimedOut);
        let (later, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert(
            "send",
            2u32,
            resolver,
            Some(now + Duration::from_secs(60)),
            || Reason::TimedOut,
        );
        let (forever, resolver) = Op::<Reason>::__rillet_pair();
        cell.insert("send", 3u32, resolver, None, || Reason::TimedOut);

        cell.expire(now);
        assert_eq!(due.state().failure(), Some(&Reason::TimedOut));
        assert!(later.state().is_pending());
        assert!(forever.state().is_pending());
        assert_eq!(cell.next_deadline(), Some(now + Duration::from_secs(60)));
    }
}
