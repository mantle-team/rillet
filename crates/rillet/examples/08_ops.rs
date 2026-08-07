//! Ops: commands whose outcome arrives later.
//!
//! An `op` handler's handle method returns an `Op` the moment the command
//! is enqueued; the caller reads it at its own pace or waits for the
//! outcome. A handler that knows the outcome returns a `Result`. A handler
//! whose outcome arrives later declares an enqueue fn and an execute
//! method: the enqueue fn names the operation's key and deadline, and the
//! outcome is delivered by whichever handler calls `succeed_<op>` or
//! `fail_<op>` with the key. An operation not concluded by its deadline
//! fails with the op's declared timeout reason.
//!
//! Run with: cargo run --example 08_ops

use std::time::{Duration, Instant};

use rillet::{OpState, Start};

/// Why a submission was rejected.
#[derive(Clone, Debug, PartialEq)]
enum Rejected {
    Odd,
}

/// A store accepting even numbers.
#[rillet::service]
struct Store {
    #[rillet(default)]
    accepted: Vec<u32>,
}

#[rillet::handlers]
impl Store {
    // The outcome is known inside the handler: return it.
    #[rillet(op)]
    fn submit(&mut self, item: u32) -> Result<(), Rejected> {
        if item.is_multiple_of(2) {
            self.accepted.push(item);
            Ok(())
        } else {
            Err(Rejected::Odd)
        }
    }
}

/// Why a message was not delivered.
#[derive(Clone, Debug, PartialEq)]
enum Failure {
    Refused,
    TimedOut,
}

/// A courier delivering messages and awaiting receipts.
#[rillet::service]
struct Courier {}

#[rillet::handlers]
impl Courier {
    // The outcome arrives later. The enqueue fn runs on the caller's
    // thread and names the operation's key and deadline; `dispatch` runs
    // when the command reaches the front of the queue.
    #[rillet(op(execute = dispatch, timeout = Failure::TimedOut))]
    fn send(id: u32, deadline: Instant) -> Start<u32, Failure> {
        Start::new(id).deadline(deadline)
    }

    fn dispatch(&mut self, _id: u32) {
        // The message would go out to a peer here.
    }

    // The peer's receipt concludes the operation under its key. A receipt
    // for an unknown or expired id concludes nothing.
    #[rillet(command)]
    fn receive_receipt(&mut self, id: u32, accepted: bool) {
        if accepted {
            self.succeed_send(&id);
        } else {
            self.fail_send(&id, Failure::Refused);
        }
    }
}

fn main() {
    let store = Store::new().spawn();

    // The command is fire-and-forget; the Op is where its outcome lands.
    let op = store.submit(2);

    // Reading the state never blocks, and the outcome may not be in yet.
    println!("just after sending: pending = {}", op.state().is_pending());

    // `concluded` waits for the outcome and returns the final state.
    assert!(matches!(
        *smol::block_on(op.concluded()),
        OpState::Done { .. }
    ));
    println!("2 was accepted");

    let op = store.submit(3);
    assert_eq!(
        smol::block_on(op.concluded()).failure(),
        Some(&Rejected::Odd)
    );
    println!("3 was rejected: {:?}", Rejected::Odd);

    // An operation can never hang: once the service is gone, the dropped
    // command concludes it as Lost.
    store.cancel().wait();
    let op = store.submit(4);
    assert!(matches!(
        *smol::block_on(op.concluded()),
        OpState::Lost { .. }
    ));
    println!("4 was lost");

    let courier = Courier::new().spawn();

    let answered = courier.send(1, Instant::now() + Duration::from_millis(100));
    let refused = courier.send(2, Instant::now() + Duration::from_millis(100));
    let unanswered = courier.send(3, Instant::now() + Duration::from_millis(100));

    // The peer answers two of the three messages.
    courier.receive_receipt(1, true);
    courier.receive_receipt(2, false);

    assert!(matches!(
        *smol::block_on(answered.concluded()),
        OpState::Done { .. }
    ));
    println!("message 1 was delivered");

    assert_eq!(
        smol::block_on(refused.concluded()).failure(),
        Some(&Failure::Refused)
    );
    println!("message 2 was refused");

    // No receipt for message 3: its deadline passes and the op fails with
    // the declared timeout reason, with no code in the service driving it.
    assert_eq!(
        smol::block_on(unanswered.concluded()).failure(),
        Some(&Failure::TimedOut)
    );
    println!("message 3 timed out");

    courier.cancel().wait();
}
