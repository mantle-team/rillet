//! Cancellation token for cooperative task shutdown.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use event_listener::{Event, EventListener};

/// A token for cooperative cancellation of async tasks.
///
/// Clone the token to share it across tasks. When `cancel()` is called,
/// all tasks waiting on `cancelled()` will be woken.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

struct Inner {
    cancelled: AtomicBool,
    event: Event,
}

impl CancellationToken {
    /// Creates a new cancellation token.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                event: Event::new(),
            }),
        }
    }

    /// Triggers cancellation.
    ///
    /// All futures returned by `cancelled()` will complete.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.event.notify(usize::MAX);
    }

    /// Returns true if cancellation has been triggered.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Returns a future that completes when cancellation is triggered.
    ///
    /// Dropping the future deregisters its wait.
    pub fn cancelled(&self) -> CancelledFuture {
        CancelledFuture {
            inner: self.inner.clone(),
            listener: None,
        }
    }

    /// Returns a clone of this token; cancelling either cancels both.
    pub fn child_token(&self) -> Self {
        self.clone()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Future that completes when a cancellation token is triggered.
pub struct CancelledFuture {
    inner: Arc<Inner>,
    listener: Option<EventListener>,
}

impl Future for CancelledFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        loop {
            if this.inner.cancelled.load(Ordering::SeqCst) {
                this.listener = None;
                return Poll::Ready(());
            }
            match &mut this.listener {
                // A notification sent before listen() is not delivered to
                // the listener, so the flag is rechecked after registering.
                None => this.listener = Some(this.inner.event.listen()),
                Some(listener) => match Pin::new(listener).poll(cx) {
                    Poll::Ready(()) => this.listener = None,
                    Poll::Pending => return Poll::Pending,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;

    #[test]
    fn completes_immediately_when_already_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        block_on(token.cancelled());
        assert!(token.is_cancelled());
    }

    #[test]
    fn wakes_across_threads() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = std::thread::spawn(move || block_on(waiter.cancelled()));
        std::thread::sleep(std::time::Duration::from_millis(20));
        token.cancel();
        handle.join().unwrap();
    }

    #[test]
    fn abandoned_waits_do_not_accumulate_registrations() {
        let token = CancellationToken::new();

        // Poll-and-drop, as the generated service loop does per iteration.
        for _ in 0..10_000 {
            let fut = token.cancelled();
            let mut fut = std::pin::pin!(fut);
            let waker = std::task::Waker::noop();
            let mut cx = Context::from_waker(waker);
            assert!(fut.as_mut().poll(&mut cx).is_pending());
        }

        assert_eq!(token.inner.event.total_listeners(), 0);

        // A live waiter still gets woken.
        let waiter = token.clone();
        let handle = std::thread::spawn(move || block_on(waiter.cancelled()));
        std::thread::sleep(std::time::Duration::from_millis(20));
        token.cancel();
        handle.join().unwrap();
    }
}
