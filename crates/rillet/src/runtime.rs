//! Runtime support the generated code calls into.

pub use crate::CancellationToken;
pub use crate::{SmolSpawner, Spawner, TaskHandle};
pub use async_channel as mpsc;
pub use futures;
pub use futures::FutureExt;
pub use std::sync::{Arc, Mutex, RwLock};

/// Send a command from a generated handle method, returning whether it
/// was delivered.
///
/// Panics when the queue is full; a send after the service has shut down
/// is a no-op returning false.
pub fn send_command<C>(tx: &mpsc::Sender<C>, command: C, method: &'static str) -> bool {
    match tx.try_send(command) {
        Ok(()) => true,
        // The service has shut down; the command is dropped.
        Err(mpsc::TrySendError::Closed(_)) => false,
        Err(mpsc::TrySendError::Full(_)) => {
            panic!("command queue full: {method}");
        }
    }
}

/// A service's spawned task handles and their completion state.
#[derive(Default)]
pub struct TaskSet {
    inner: Mutex<TaskSetState>,
    done_signal: std::sync::Condvar,
}

#[derive(Default)]
struct TaskSetState {
    handles: Vec<Box<dyn TaskHandle>>,
    done: bool,
}

impl TaskSet {
    /// Creates an empty task set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a task handle to the set.
    pub fn push(&self, handle: Box<dyn TaskHandle>) {
        self.inner.lock().unwrap().handles.push(handle);
    }

    /// Returns true once a waiter has seen every task complete.
    fn is_done(&self) -> bool {
        self.inner.lock().unwrap().done
    }

    /// Blocks until every task in the set completes.
    ///
    /// The waiter that takes the handles receives any task panic; other
    /// waiters wake once the set is done.
    fn wait(&self) {
        let taken = std::mem::take(&mut self.inner.lock().unwrap().handles);

        if taken.is_empty() {
            let mut state = self.inner.lock().unwrap();
            while !state.done {
                state = self.done_signal.wait(state).unwrap();
            }
            return;
        }

        // The guard marks the set done even when a task panic propagates
        // out of block_on, so the other waiters wake either way.
        struct DoneGuard<'a>(&'a TaskSet);
        impl Drop for DoneGuard<'_> {
            fn drop(&mut self) {
                self.0.inner.lock().unwrap().done = true;
                self.0.done_signal.notify_all();
            }
        }
        let _guard = DoneGuard(self);

        for handle in taken {
            handle.block_on();
        }
    }
}

/// Handle for waiting on task completion after shutdown.
pub struct TaskCompletion {
    sets: Vec<Arc<TaskSet>>,
}

impl TaskCompletion {
    /// Creates a new TaskCompletion over a shared task set.
    pub fn new(set: Arc<TaskSet>) -> Self {
        Self { sets: vec![set] }
    }

    /// Blocks until all tasks complete.
    ///
    /// A task panic propagates to the waiter that consumed the task's
    /// handle; concurrent waiters on the same set return without it.
    pub fn wait(&self) {
        for set in &self.sets {
            set.wait();
        }
    }

    /// Returns true once every set has been waited to completion.
    pub fn is_complete(&self) -> bool {
        self.sets.iter().all(|set| set.is_done())
    }

    /// Combines multiple completions into one that waits on all their tasks.
    pub fn join<I>(completions: I) -> Self
    where
        I: IntoIterator<Item = TaskCompletion>,
    {
        Self {
            sets: completions.into_iter().flat_map(|c| c.sets).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{mpsc, send_command};

    #[test]
    fn delivers_when_open() {
        let (tx, rx) = mpsc::bounded(1);
        assert!(send_command(&tx, 7, "do_thing"));
        assert_eq!(rx.try_recv(), Ok(7));
    }

    #[test]
    fn drops_when_closed() {
        let (tx, rx) = mpsc::bounded(1);
        drop(rx);
        assert!(!send_command(&tx, 7, "do_thing"));
    }

    #[test]
    #[should_panic(expected = "command queue full: do_thing")]
    fn panics_when_full() {
        let (tx, _rx) = mpsc::bounded(1);
        send_command(&tx, 7, "do_thing");
        send_command(&tx, 8, "do_thing");
    }
}
