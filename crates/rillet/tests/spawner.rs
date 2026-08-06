mod support;

use std::future::Future;

use rillet::{CancellationToken, Spawner, TaskHandle};
use support::wait_for;

/// A spawner that runs each task on its own thread.
#[derive(Clone)]
struct ThreadSpawner;

struct ThreadHandle(std::thread::JoinHandle<()>);

impl TaskHandle for ThreadHandle {
    fn block_on(self: Box<Self>) {
        self.0.join().unwrap();
    }
}

impl Spawner for ThreadSpawner {
    type Handle = ThreadHandle;

    fn spawn<F>(&self, future: F) -> Self::Handle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        ThreadHandle(std::thread::spawn(move || {
            futures_lite::future::block_on(future)
        }))
    }
}

#[rillet::service]
pub struct Pinger {
    #[rillet(get, default)]
    pongs: u32,
}

#[rillet::handlers]
impl Pinger {
    #[rillet(command)]
    fn ping(&mut self) {
        self.pongs += 1;
    }

    #[rillet(task)]
    async fn ping_once(handle: PingerHandle, _cancel: CancellationToken) {
        handle.ping();
    }
}

#[test]
fn services_run_on_a_custom_spawner() {
    let pinger = Pinger::new().spawn_with(ThreadSpawner);

    pinger.ping();
    wait_for("the command and the task to run", || pinger.pongs() == 2);

    pinger.cancel().wait();
}
