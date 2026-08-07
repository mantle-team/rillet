//! Tasks: background work a service owns, with cooperative shutdown.
//!
//! A `#[rillet(task)]` method is spawned alongside the service and
//! receives the service's handle and a cancellation token. Tasks mutate
//! state the same way everyone else does: by sending commands.
//!
//! Run with: cargo run --example 05_tasks

use std::time::Duration;

use futures::FutureExt;
use rillet::CancellationToken;

/// A heartbeat counter driven by its own timer task.
#[rillet::service]
struct Heart {
    #[rillet(get, default)]
    beats: u32,
}

#[rillet::handlers]
impl Heart {
    #[rillet(command)]
    fn beat(&mut self) {
        self.beats += 1;
    }

    /// Sends one `beat` command every 50 milliseconds until cancelled.
    #[rillet(task)]
    async fn beat_periodically(handle: HeartHandle, cancel: CancellationToken) {
        loop {
            let mut cancelled = std::pin::pin!(cancel.cancelled().fuse());
            let mut tick = std::pin::pin!(smol::Timer::after(Duration::from_millis(50)).fuse());
            futures::select! {
                _ = cancelled => break,
                _ = tick => handle.beat(),
            }
        }
    }
}

/// A list filled by a task that was handed its data at spawn.
#[rillet::service]
struct Loader {
    #[rillet(get, default)]
    items: Vec<u32>,
}

#[rillet::handlers]
impl Loader {
    #[rillet(command)]
    fn add(&mut self, item: u32) {
        self.items.push(item);
    }

    /// Sends one `add` command per item of its source, then ends.
    ///
    /// A task with parameters beyond the handle and token receives them
    /// at spawn, through the generated `spawn_add_each(...)` builder.
    #[rillet(task)]
    async fn add_each(handle: LoaderHandle, _cancel: CancellationToken, source: Vec<u32>) {
        for item in source {
            handle.add(item);
        }
    }
}

fn main() {
    let heart = Heart::new().spawn();
    // `spawn_add_each` provides the task's `source` parameter.
    let loader = Loader::new().spawn_add_each(vec![1, 2, 3]).spawn();

    while heart.beats() < 5 {
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("beats: {}", heart.beats());

    while loader.items().len() < 3 {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("loaded: {:?}", loader.items());

    // Cancel, then wait for the task and the service loop to finish.
    heart.cancel().wait();

    // Confirm no beats arrive after cancellation.
    let after = heart.beats();
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(heart.beats(), after);
    println!("stopped at {} beats", after);

    loader.cancel();
}
