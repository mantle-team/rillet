//! Direct methods: synchronous calls on the caller's thread.
//!
//! A `direct` method reads under the service's read lock; a `direct_mut`
//! method writes under its write lock and republishes the view before
//! returning. Both skip the command queue, and the caller holds the
//! service's lock for the duration, so keep the bodies short.
//!
//! Run with: cargo run --example 06_direct

use rillet::CheapClone;

/// A snapshot of the gauge.
#[derive(Clone, PartialEq, CheapClone)]
struct GaugeView {
    level: i64,
}

/// A gauge holding one level.
#[rillet::service(view = GaugeView)]
struct Gauge {
    #[rillet(default)]
    level: i64,
}

impl Gauge {
    fn view(&self) -> GaugeView {
        GaugeView { level: self.level }
    }
}

#[rillet::handlers]
impl Gauge {
    #[rillet(direct)]
    fn level_squared(&self) -> i64 {
        self.level * self.level
    }

    #[rillet(direct_mut)]
    fn set_level(&mut self, level: i64) -> i64 {
        self.level = level;
        self.level
    }
}

fn main() {
    let gauge = Gauge::new().spawn();

    println!("squared: {}", gauge.level_squared());

    // Mutate and receive the new level in one call.
    let now = gauge.set_level(-4);
    println!("set_level returned {now}");
    // The republished view is already current; nothing is polled.
    println!("view shows {}", gauge.view().level);
    println!("squared: {}", gauge.level_squared());

    gauge.cancel();
}
