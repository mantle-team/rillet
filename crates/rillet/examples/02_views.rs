//! Views: lock-free snapshots of a service's state.
//!
//! A service that declares `view = SomeView` publishes a view snapshot after
//! every mutation. Readers can load the latest snapshot without taking a
//! lock, and watchers wake only when it changed, always seeing the newest
//! one. A view handle narrows a full handle to loading and watching, for
//! consumers that must not send commands or reach the service's locks.
//!
//! Run with: cargo run --example 02_views

use futures_lite::future::block_on;
use rillet::CheapClone;

/// A snapshot of the thermometer.
#[derive(Clone, PartialEq, CheapClone)]
struct ThermoView {
    celsius: f64,
    fahrenheit: f64,
}

/// A thermometer holding one temperature.
#[rillet::service(view = ThermoView)]
struct Thermometer {
    #[rillet(default = 20.0)]
    celsius: f64,
}

impl Thermometer {
    /// Builds the view published after each mutation.
    ///
    /// Runs under the service's write lock, so both fields derive from the
    /// same state and no reader can see them disagree.
    fn view(&self) -> ThermoView {
        ThermoView {
            celsius: self.celsius,
            fahrenheit: self.celsius * 9.0 / 5.0 + 32.0,
        }
    }
}

#[rillet::handlers]
impl Thermometer {
    #[rillet(command)]
    fn set(&mut self, celsius: f64) {
        self.celsius = celsius;
    }
}

fn main() {
    let thermometer = Thermometer::new().spawn();

    // The view is seeded from the initial state, show the initial value.
    println!("initial: {}°C", thermometer.view().celsius);

    // Create a view watcher.
    let mut watch = thermometer.watch_view();

    // Call a command which mutates the service.
    thermometer.set(21.5);

    let view = block_on(watch.changed());
    println!("changed: {}°C = {}°F", view.celsius, view.fahrenheit);

    // Set the same value (deduped, nothing published), then a new one;
    // the watcher wakes once, with the newest view.
    thermometer.set(21.5);
    thermometer.set(30.0);
    let view = block_on(watch.changed());
    println!("changed: {}°C (21.5°C repeat was skipped)", view.celsius);

    // Convert the full handle into a read-only view handle.
    let reader: ThermometerViewHandle = thermometer.clone().into();
    println!("via view handle: {}°C", reader.view().celsius);

    thermometer.cancel();
}
