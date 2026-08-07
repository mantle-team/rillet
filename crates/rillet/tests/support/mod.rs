use std::future::Future;
use std::time::{Duration, Instant};

/// Blocks on the future, panicking after five seconds.
#[allow(dead_code)]
pub fn wait_on<T>(what: &str, fut: impl Future<Output = T>) -> T {
    futures_lite::future::block_on(futures_lite::future::or(fut, async {
        async_io::Timer::after(Duration::from_secs(5)).await;
        panic!("timed out waiting for {what}");
    }))
}

/// Spins until the condition holds, panicking after five seconds.
pub fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
