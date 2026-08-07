//! Executor-agnostic task spawning.
//!
//! Every service spawns its loop and its tasks through a [`Spawner`]. The
//! default, [`SmolSpawner`], uses smol's global executor.

use std::future::Future;

/// A handle to a spawned task that can be blocked on.
pub trait TaskHandle: Send + 'static {
    /// Blocks the current thread until the task completes.
    fn block_on(self: Box<Self>);
}

/// A source of spawned tasks.
///
/// Each service that spawns tasks holds its own clone. Implement it to run
/// services on another executor.
pub trait Spawner: Clone + Send + Sync + 'static {
    /// The handle returned when spawning a task.
    type Handle: TaskHandle;

    /// Spawns a future on this spawner's executor.
    fn spawn<F>(&self, future: F) -> Self::Handle
    where
        F: Future<Output = ()> + Send + 'static;
}

/// The default spawner, using smol's global executor.
#[derive(Clone, Default)]
pub struct SmolSpawner;

impl Spawner for SmolSpawner {
    type Handle = smol::Task<()>;

    fn spawn<F>(&self, future: F) -> Self::Handle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::spawn(future)
    }
}

impl TaskHandle for smol::Task<()> {
    fn block_on(self: Box<Self>) {
        futures_lite::future::block_on(*self);
    }
}
