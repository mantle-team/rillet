//! Gauges inside views: per-entity signals whose cardinality follows the
//! structure.
//!
//! Who is in the room is structure: it changes on join and leave, and the
//! view publishes for it. How loud each peer is, is a signal: its cell
//! lives inside the peer's view entry, the producer stores into it at its
//! own rate, and readers sample it per frame. Cell equality is identity,
//! so stored values never republish the view; a membership change does.
//!
//! Run with: cargo run --example 10_gauges_in_views

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rillet::gauge::Atomic;

#[derive(Clone, PartialEq, rillet::CheapClone)]
struct Peer {
    name: &'static str,
    level: Atomic<f32>,
}

#[derive(Clone, PartialEq, rillet::CheapClone)]
struct RoomView {
    peers: Arc<Vec<Peer>>,
}

/// A call whose per-peer levels live inside its view.
#[rillet::service(view = RoomView)]
struct Room {
    #[rillet(default)]
    peers: Vec<Peer>,
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
    /// Joining creates the peer and its level cell.
    #[rillet(command)]
    fn join(&mut self, name: &'static str) {
        self.peers.push(Peer {
            name,
            level: Atomic::default(),
        });
    }
}

fn main() {
    let room = Room::new().spawn();

    room.join("ada");
    room.join("grace");
    let view = room.wait_view(|v| v.peers.len() == 2).expect("cancelled");

    // The producing side: one mock decode thread per peer stores that
    // peer's level at its own rate. A real one would be the network or
    // audio path holding the cell clone.
    for (offset, peer) in view.peers.iter().enumerate() {
        let level = peer.level.clone();
        thread::spawn(move || {
            for tick in 0..100u32 {
                level.store(((tick as usize + offset * 5) % 10) as f32 / 10.0);
                thread::sleep(Duration::from_millis(3));
            }
        });
    }

    // The consuming side: each frame walks the structure it was last told
    // about and samples the live signal inside each entry.
    for frame in 1..=3 {
        thread::sleep(Duration::from_millis(30));
        let view = room.view();
        println!("frame {frame}:");
        for peer in view.peers.iter() {
            println!("  {} {:.1}", peer.name, peer.level.load());
        }
    }

    room.cancel().wait();
}
