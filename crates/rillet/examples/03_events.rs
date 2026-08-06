//! Events: typed broadcasts between services and to the outside.
//!
//! A service declares what it emits. Another service subscribes by
//! holding its handle in a field and marking a handler
//! `#[rillet(from = field)]`; external code subscribes through the
//! generated `on_*` methods. Every subscriber receives every event
//! emitted after it subscribed.
//!
//! Run with: cargo run --example 03_events

use std::time::Duration;

use rillet::Event;

/// A message accepted into the room.
#[derive(Clone, Event)]
struct MessagePosted {
    author: String,
    length: usize,
}

/// A chat room accepting messages.
#[rillet::service]
#[rillet(emits = [MessagePosted])]
struct Room {
    #[rillet(get, default)]
    messages: u32,
}

#[rillet::handlers]
impl Room {
    #[rillet(command)]
    fn post(&mut self, author: String, text: String) {
        self.messages += 1;
        self.emit_message_posted(MessagePosted {
            author,
            length: text.len(),
        });
    }
}

/// A running total over everything posted to one room.
#[rillet::service]
struct Stats {
    room: RoomHandle,

    #[rillet(get, default)]
    total_length: usize,
}

#[rillet::handlers]
impl Stats {
    #[rillet(from = room)]
    fn on_message_posted(&mut self, event: MessagePosted) {
        self.total_length += event.length;
    }
}

fn main() {
    let room = Room::new().spawn();
    let stats = Stats::new(room.clone()).spawn();

    // Subscribe from outside the services.
    let mut posts = room.on_message_posted();

    room.post("ada".into(), "hello".into());
    room.post("grace".into(), "compilers!".into());

    for _ in 0..2 {
        let event = posts.recv().expect("the room is still running");
        println!("{} posted {} bytes", event.author, event.length);
    }

    // Poll until Stats has processed both events.
    while stats.total_length() < 15 {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("total bytes posted: {}", stats.total_length());

    // Print the emitter's counters.
    println!(
        "events published: {}, current subscribers: {}",
        room.message_posted_published(),
        room.message_posted_subscriber_count()
    );

    stats.cancel();
    room.cancel();
}
