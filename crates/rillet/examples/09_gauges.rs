//! Gauges: lock-free latest-value cells for continuous signals.
//!
//! A gauge carries a signal whose history does not matter, like an audio
//! level. The producer stores the latest value without locking,
//! allocating, or waking anyone; readers sample the current value
//! whenever they want it. A field marked `#[rillet(gauge)]` starts at its
//! `Default` value, and the handle gets a sampler named after the field
//! that never touches the state lock.
//!
//! Run with: cargo run --example 09_gauges

use std::time::Duration;

use rillet::CancellationToken;
use rillet::gauge::Atomic;

/// A capture pipeline reporting its input level.
#[rillet::service]
struct Microphone {
    #[rillet(gauge)]
    level: Atomic<f32>,
}

#[rillet::handlers]
impl Microphone {
    /// Hands out a clone of the cell; clones share their storage, so from
    /// here on the producer stores without any lock.
    #[rillet(direct)]
    fn level_cell(&self) -> Atomic<f32> {
        self.level.clone()
    }

    // The producer. A real one would be an OS audio callback holding the
    // cell clone; this one synthesizes a level every few milliseconds.
    #[rillet(task)]
    async fn capture(handle: MicrophoneHandle, cancel: CancellationToken) {
        let level = handle.level_cell();
        let mut tick = 0u32;
        while !cancel.is_cancelled() {
            tick += 1;
            level.store((tick as f32 / 10.0).sin().abs());
            smol::Timer::after(Duration::from_millis(5)).await;
        }
    }
}

fn main() {
    let mic = Microphone::new().spawn();

    // The consumer samples whenever it wants the value, here once per
    // pretend frame. There is nothing to subscribe to and nothing wakes
    // the reader; a missed value is simply never seen.
    for frame in 1..=5 {
        std::thread::sleep(Duration::from_millis(40));
        println!("frame {frame}: level {:.2}", mic.level());
    }

    mic.cancel().wait();
}
