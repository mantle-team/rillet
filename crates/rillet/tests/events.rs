mod support;

use rillet::Event;
use support::{wait_for, wait_on};

#[derive(Clone, Event)]
pub struct Chirp {
    pub loudness: u32,
}

#[rillet::service]
#[rillet(emits = [Chirp])]
pub struct Bird {
    #[rillet(get, default)]
    chirps: u32,
}

#[rillet::handlers]
impl Bird {
    #[rillet(command)]
    fn chirp(&mut self, loudness: u32) {
        self.chirps += 1;
        self.emit_chirp(Chirp { loudness });
    }
}

#[rillet::service]
pub struct Listener {
    bird: BirdHandle,

    #[rillet(get, default)]
    heard: u32,
}

#[rillet::handlers]
impl Listener {
    #[rillet(from = bird)]
    fn on_chirp(&mut self, event: Chirp) {
        self.heard += event.loudness;
    }
}

#[test]
fn external_subscribers_receive_events() {
    let bird = Bird::new().spawn();
    let mut chirps = bird.on_chirp();

    bird.chirp(3);
    bird.chirp(4);

    assert_eq!(
        wait_on("first chirp", chirps.next()).map(|c| c.loudness),
        Some(3)
    );
    assert_eq!(
        wait_on("second chirp", chirps.next()).map(|c| c.loudness),
        Some(4)
    );
    bird.cancel();
}

#[test]
fn subscribers_only_see_events_after_subscribing() {
    let bird = Bird::new().spawn();
    bird.chirp(1);
    wait_for("first chirp to process", || bird.chirps() == 1);

    let mut chirps = bird.on_chirp();
    bird.chirp(2);
    assert_eq!(
        wait_on("late chirp", chirps.next()).map(|c| c.loudness),
        Some(2)
    );
    bird.cancel();
}

#[test]
fn service_to_service_subscription_runs_handlers() {
    let bird = Bird::new().spawn();
    let listener = Listener::new(bird.clone()).spawn();

    bird.chirp(5);
    bird.chirp(7);

    wait_for("listener to hear both chirps", || listener.heard() == 12);
    listener.cancel();
    bird.cancel();
}

#[rillet::service]
#[rillet(emits = [Chirp])]
pub struct Parrot {
    bird: BirdHandle,
}

#[rillet::handlers]
impl Parrot {
    #[rillet(command)]
    fn preen(&mut self) {}

    #[rillet(from = bird)]
    fn on_chirp(&mut self, event: Chirp) {
        self.emit_chirp(event);
    }
}

#[test]
fn services_outlive_their_handles() {
    let bird = Bird::new().spawn();
    let parrot = Parrot::new(bird.clone()).spawn();
    let mut echoes = parrot.on_chirp();

    parrot.preen();
    drop(parrot);
    bird.chirp(6);

    assert_eq!(
        wait_on("echoed chirp", echoes.next()).map(|c| c.loudness),
        Some(6)
    );
    bird.cancel();
}

#[derive(Clone, Event)]
pub struct Squawk {
    pub volume: u32,
}

#[rillet::service]
#[rillet(emits = [Chirp, Squawk])]
pub struct Macaw {}

#[rillet::handlers]
impl Macaw {
    #[rillet(command)]
    fn chirp(&mut self, loudness: u32) {
        self.emit_chirp(Chirp { loudness });
    }

    #[rillet(command)]
    fn squawk(&mut self, volume: u32) {
        self.emit_squawk(Squawk { volume });
    }
}

#[test]
fn each_event_type_has_its_own_channel() {
    let macaw = Macaw::new().spawn();
    let mut chirps = macaw.on_chirp();
    let mut squawks = macaw.on_squawk();

    macaw.chirp(3);
    macaw.squawk(9);
    macaw.chirp(4);

    assert_eq!(
        wait_on("first chirp", chirps.next()).map(|c| c.loudness),
        Some(3)
    );
    assert_eq!(
        wait_on("second chirp", chirps.next()).map(|c| c.loudness),
        Some(4)
    );
    assert_eq!(wait_on("squawk", squawks.next()).map(|s| s.volume), Some(9));

    wait_for("per-type publish counters", || {
        macaw.chirp_published() == 2 && macaw.squawk_published() == 1
    });
    assert_eq!(macaw.chirp_subscriber_count(), 1);
    assert_eq!(macaw.squawk_subscriber_count(), 1);
    macaw.cancel();
}

#[rillet::service(event_capacity = 2)]
#[rillet(emits = [Chirp])]
pub struct QuietBird {}

#[rillet::handlers]
impl QuietBird {
    #[rillet(command)]
    fn chirp(&mut self) {
        self.emit_chirp(Chirp { loudness: 1 });
    }
}

#[test]
fn event_capacity_sizes_the_channel() {
    let bird = QuietBird::new().spawn();
    assert_eq!(bird.on_chirp().capacity(), 2);
    bird.cancel();
}

#[test]
fn emitter_counts_published_events_and_subscribers() {
    let bird = Bird::new().spawn();
    assert_eq!(bird.chirp_subscriber_count(), 0);

    let _rx = bird.on_chirp();
    assert_eq!(bird.chirp_subscriber_count(), 1);

    bird.chirp(1);
    wait_for("publish counter to tick", || bird.chirp_published() == 1);
    bird.cancel();
}
