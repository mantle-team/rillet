mod support;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use rillet::gauge::Atomic;
use support::wait_for;

/// A service with a service-level signal.
#[rillet::service]
struct Microphone {
    #[rillet(gauge)]
    level: Atomic<f32>,
}

#[rillet::handlers]
impl Microphone {
    #[rillet(command)]
    fn calibrate(&mut self, level: f32) {
        self.level.store(level);
    }

    #[rillet(direct_mut)]
    fn hold(&mut self, held: Sender<()>, release: Receiver<()>) {
        self.level.store(9.0);
        held.send(()).expect("test main dropped the held receiver");
        release
            .recv()
            .expect("test main dropped the release sender");
    }
}

#[test]
fn a_gauge_starts_at_the_value_default() {
    let mic = Microphone::new().spawn();
    assert_eq!(mic.level(), 0.0);
    mic.cancel().wait();
}

#[test]
fn the_handle_samples_the_latest_store() {
    let mic = Microphone::new().spawn();
    mic.calibrate(0.75);
    wait_for("the store to land", || mic.level() == 0.75);
    mic.cancel().wait();
}

#[test]
fn sampling_needs_no_state_lock() {
    let mic = Microphone::new().spawn();
    let (held_tx, held_rx) = channel();
    let (release_tx, release_rx) = channel();

    let holder = {
        let mic = mic.clone();
        std::thread::spawn(move || mic.hold(held_tx, release_rx))
    };

    // The handler now holds the state write lock; sampling still returns.
    held_rx.recv().expect("holder to reach the handler");
    assert_eq!(mic.level(), 9.0);

    release_tx.send(()).expect("holder to still be blocked");
    holder.join().expect("holder to finish");
    mic.cancel().wait();
}

#[derive(Clone, PartialEq, rillet::CheapClone)]
struct Peer {
    id: u32,
    level: Atomic<f32>,
}

#[derive(Clone, PartialEq, rillet::CheapClone)]
struct RoomView {
    peers: Arc<Vec<Peer>>,
}

/// A service whose per-peer signals live inside its view.
#[rillet::service(view = RoomView)]
struct Room {
    #[rillet(default)]
    peers: Vec<Peer>,

    #[rillet(gauge)]
    mix_level: Atomic<f32>,
}

impl Room {
    fn view(&self) -> RoomView {
        RoomView {
            peers: Arc::new(self.peers.clone()),
        }
    }
}

#[rillet::handlers]
impl Room {
    #[rillet(command)]
    fn join(&mut self, id: u32) {
        self.peers.push(Peer {
            id,
            level: Atomic::default(),
        });
    }

    #[rillet(command)]
    fn set_mix(&mut self, level: f32) {
        self.mix_level.store(level);
    }
}

#[test]
fn the_view_handle_samples_gauges() {
    let room = Room::new().spawn();
    let reader: RoomViewHandle = room.clone().into();
    room.set_mix(0.5);
    wait_for("the store to land", || reader.mix_level() == 0.5);
    room.cancel().wait();
}

#[test]
fn a_stored_value_does_not_republish_the_view() {
    let room = Room::new().spawn();
    room.join(1);
    let view = room.wait_view(|v| v.peers.len() == 1).expect("cancelled");

    view.peers[0].level.store(0.4);

    // A recompute after an effect-free mutation finds the view equal by
    // cell identity and does not republish it.
    room.set_mix(0.0);
    wait_for("the mutation to process", || {
        room.aggregate_stats().total_processed == 2
    });
    assert!(Arc::ptr_eq(&view, &room.view()));
    assert_eq!(room.view().peers[0].level.load(), 0.4);
    room.cancel().wait();
}

#[test]
fn a_membership_change_republishes_and_keeps_the_cells() {
    let room = Room::new().spawn();
    room.join(1);
    let before = room.wait_view(|v| v.peers.len() == 1).expect("cancelled");
    before.peers[0].level.store(0.4);

    room.join(2);
    let after = room.wait_view(|v| v.peers.len() == 2).expect("cancelled");

    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(after.peers[0].level.load(), 0.4);
    assert_eq!(after.peers[0].level, before.peers[0].level);
    room.cancel().wait();
}
