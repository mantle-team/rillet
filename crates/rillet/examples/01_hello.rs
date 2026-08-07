//! The smallest possible service: one command, one readable field.
//!
//! A service owns its state and a handle reaches it from outside:
//! commands queue onto the service's channel and run one at a time, in
//! order; reads clone a field under a read lock.
//!
//! Run with: cargo run --example 01_hello

use std::time::Duration;

/// A greeter counting the greetings it has given.
#[rillet::service]
struct Greeter {
    name: String,

    #[rillet(get, default)]
    greetings: u32,
}

#[rillet::handlers]
impl Greeter {
    #[rillet(command)]
    fn greet(&mut self, whom: String) {
        self.greetings += 1;
        println!("[{}] hello, {whom}!", self.name);
    }
}

fn main() {
    // `new()` takes the fields not marked `#[rillet(default)]`.
    let greeter = Greeter::new("greeter-1".into())
        // spawn starts the service and returns a cloneable handle.
        .spawn();

    greeter.greet("world".into());
    greeter.greet("rillet".into());

    // Poll the getter until both commands have been processed.
    while greeter.greetings() < 2 {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("greetings given: {}", greeter.greetings());

    greeter.cancel();
}
