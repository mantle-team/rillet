//! Runtime support the generated code calls into.

pub use crate::CancellationToken;
pub use crate::{SmolSpawner, Spawner, TaskHandle};
pub use async_broadcast as broadcast;
pub use async_channel as mpsc;
pub use async_io::Timer;
pub use futures;
pub use futures::FutureExt;
pub use std::sync::{Arc, Mutex, RwLock};

use std::fmt;
use std::time::Duration;

/// Send a command from a generated handle method.
///
/// Panics when the queue is full; a send after the service has shut down
/// is a no-op.
pub fn send_command<C>(tx: &mpsc::Sender<C>, command: C, method: &'static str) {
    match tx.try_send(command) {
        Ok(()) => {}
        // The service has shut down; the command is dropped.
        Err(mpsc::TrySendError::Closed(_)) => {}
        Err(mpsc::TrySendError::Full(_)) => {
            panic!("command queue full: {method}");
        }
    }
}

/// Handle for waiting on task completion after shutdown.
pub struct TaskCompletion {
    handles: Arc<Mutex<Vec<Box<dyn TaskHandle>>>>,
}

impl TaskCompletion {
    /// Creates a new TaskCompletion from shared task handles.
    pub fn new(handles: Arc<Mutex<Vec<Box<dyn TaskHandle>>>>) -> Self {
        Self { handles }
    }

    /// Blocks until all tasks complete.
    ///
    /// Task panics propagate directly to this call.
    pub fn wait(&self) -> Result<(), TaskPanicked> {
        let handles: Vec<_> = self.handles.lock().unwrap().drain(..).collect();
        for handle in handles {
            handle.block_on();
        }

        Ok(())
    }

    /// Blocks until all tasks complete or the timeout expires.
    ///
    /// Task panics propagate directly to this call.
    pub fn wait_timeout(&self, duration: Duration) -> Result<(), WaitError> {
        use futures_lite::future;

        future::block_on(async {
            let wait_future = async {
                let handles: Vec<_> = self.handles.lock().unwrap().drain(..).collect();
                for handle in handles {
                    handle.block_on();
                }
            };

            let timeout_future = Timer::after(duration);

            futures_lite::future::or(
                async {
                    wait_future.await;
                    Ok(())
                },
                async {
                    timeout_future.await;
                    Err(WaitError::Timeout)
                },
            )
            .await
        })
    }

    /// Returns true when no task handles remain to wait on.
    pub fn is_complete(&self) -> bool {
        self.handles.lock().unwrap().is_empty()
    }

    /// Combines multiple completions into one that waits on all their tasks.
    pub fn join<I>(completions: I) -> Self
    where
        I: IntoIterator<Item = TaskCompletion>,
    {
        let combined = Arc::new(Mutex::new(Vec::new()));
        for completion in completions {
            let mut handles = completion.handles.lock().unwrap();
            combined.lock().unwrap().append(&mut handles);
        }
        Self { handles: combined }
    }
}

/// Error returned when one or more tasks panicked.
#[derive(Debug)]
pub struct TaskPanicked {
    /// Panic messages from failed tasks.
    pub panics: Vec<String>,
}

impl fmt::Display for TaskPanicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} task(s) panicked", self.panics.len())
    }
}

impl std::error::Error for TaskPanicked {}

/// Error returned from `wait_timeout`.
#[derive(Debug)]
pub enum WaitError {
    /// The timeout expired before all tasks completed.
    Timeout,
    /// One or more tasks panicked.
    Panicked(TaskPanicked),
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaitError::Timeout => write!(f, "timeout waiting for tasks"),
            WaitError::Panicked(p) => write!(f, "{}", p),
        }
    }
}

impl std::error::Error for WaitError {}

#[cfg(test)]
mod tests {
    use super::{mpsc, send_command};

    #[test]
    fn delivers_when_open() {
        let (tx, rx) = mpsc::bounded(1);
        send_command(&tx, 7, "do_thing");
        assert_eq!(rx.try_recv(), Ok(7));
    }

    #[test]
    fn drops_when_closed() {
        let (tx, rx) = mpsc::bounded(1);
        drop(rx);
        send_command(&tx, 7, "do_thing");
    }

    #[test]
    #[should_panic(expected = "command queue full: do_thing")]
    fn panics_when_full() {
        let (tx, _rx) = mpsc::bounded(1);
        send_command(&tx, 7, "do_thing");
        send_command(&tx, 8, "do_thing");
    }
}
