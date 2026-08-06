mod support;

use std::time::Duration;

use futures::FutureExt;
use rillet::CancellationToken;
use support::wait_for;

#[rillet::service]
pub struct Beater {
    #[rillet(get, default)]
    beats: u32,
}

#[rillet::handlers]
impl Beater {
    #[rillet(command)]
    fn beat(&mut self) {
        self.beats += 1;
    }

    #[rillet(task)]
    async fn beat_periodically(handle: BeaterHandle, cancel: CancellationToken) {
        loop {
            let mut cancelled = std::pin::pin!(cancel.cancelled().fuse());
            let mut tick = std::pin::pin!(smol::Timer::after(Duration::from_millis(5)).fuse());
            futures::select! {
                _ = cancelled => break,
                _ = tick => handle.beat(),
            }
        }
    }
}

#[rillet::service]
pub struct Loader {
    #[rillet(get, default)]
    items: Vec<u32>,
}

#[rillet::handlers]
impl Loader {
    #[rillet(command)]
    fn add(&mut self, item: u32) {
        self.items.push(item);
    }

    // A task with parameters beyond (handle, cancel) is provided its context
    // at spawn time via the generated `spawn_feed(...)` builder method.
    #[rillet(task)]
    async fn feed(handle: LoaderHandle, _cancel: CancellationToken, source: Vec<u32>) {
        for item in source {
            handle.add(item);
        }
    }
}

#[test]
fn tasks_drive_the_service_through_commands() {
    let beater = Beater::new().spawn();
    wait_for("task to beat a few times", || beater.beats() >= 3);
    beater.cancel();
}

#[test]
fn cancellation_stops_tasks_and_completion_observes_it() {
    let beater = Beater::new().spawn();
    wait_for("task to start", || beater.beats() >= 1);

    beater.cancel().wait();

    let stopped_at = beater.beats();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(beater.beats(), stopped_at);
}

#[test]
fn join_waits_on_all_completions() {
    let first = Beater::new().spawn();
    let second = Beater::new().spawn();
    wait_for("both tasks to start", || {
        first.beats() >= 1 && second.beats() >= 1
    });

    rillet::runtime::TaskCompletion::join([first.cancel(), second.cancel()]).wait();

    let stopped = (first.beats(), second.beats());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!((first.beats(), second.beats()), stopped);
}

#[test]
fn completion_reports_completeness_after_wait() {
    let beater = Beater::new().spawn();
    let completion = beater.task_completion();
    assert!(!completion.is_complete());

    beater.cancel();
    completion.wait();
    assert!(completion.is_complete());
}

#[test]
fn context_tasks_receive_their_arguments() {
    let loader = Loader::new().spawn_feed(vec![1, 2, 3]).spawn();
    wait_for("feed task to deliver items", || loader.items().len() == 3);
    assert_eq!(loader.items(), vec![1, 2, 3]);
    loader.cancel();
}
