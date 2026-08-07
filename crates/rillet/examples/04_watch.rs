//! Watching: a service reacting to another service's views.
//!
//! Where events carry what happened, views carry what is. A handler
//! marked `#[rillet(watch = field)]` runs with the newest view published
//! by that field's service: intermediate views may be skipped when the
//! watcher lags, and the newest is never missed.
//!
//! Run with: cargo run --example 04_watch

use std::sync::Arc;
use std::time::Duration;

use rillet::CheapClone;

/// A snapshot of the dial.
#[derive(Clone, PartialEq, CheapClone)]
struct DialView {
    level: u8,
}

/// A dial holding one level.
#[rillet::service(view = DialView)]
struct Dial {
    #[rillet(default)]
    level: u8,
}

impl Dial {
    fn view(&self) -> DialView {
        DialView { level: self.level }
    }
}

#[rillet::handlers]
impl Dial {
    #[rillet(command)]
    fn set(&mut self, level: u8) {
        self.level = level;
    }
}

/// A record of the dial levels this service observed.
#[rillet::service]
struct Recorder {
    dial: DialHandle,

    #[rillet(get, default)]
    observed: Vec<u8>,
}

#[rillet::handlers]
impl Recorder {
    #[rillet(watch = dial)]
    fn on_dial_view(&mut self, view: Arc<DialView>) {
        self.observed.push(view.level);
    }
}

fn main() {
    let dial = Dial::new().spawn();
    let recorder = Recorder::new(dial.clone()).spawn();

    for level in [3, 7, 9] {
        dial.set(level);
        // Pause so each publish is observed on its own rather than
        // coalesced into the next.
        std::thread::sleep(Duration::from_millis(20));
    }

    // Poll until the last set has been observed.
    while recorder.observed().last() != Some(&9) {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("observed levels: {:?}", recorder.observed());

    recorder.cancel();
    dial.cancel();
}
