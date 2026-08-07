use std::time::{Duration, Instant};

use futures_lite::future::block_on;
use rillet::{OpState, Start};

#[derive(Clone, Debug, PartialEq)]
enum Rejected {
    Odd,
}

/// A service concluding ops inside the handler.
#[rillet::service]
struct Store {
    #[rillet(get, default)]
    accepted: Vec<u32>,
}

#[rillet::handlers]
impl Store {
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

#[derive(Clone, Debug, PartialEq)]
enum Failure {
    Refused,
    TimedOut,
}

/// A service whose op outcomes arrive later, keyed by id.
#[rillet::service]
struct Courier {
    #[rillet(get, default)]
    dispatched: Vec<u32>,
}

#[rillet::handlers]
impl Courier {
    #[rillet(op(execute = dispatch, timeout = Failure::TimedOut))]
    fn send(id: u32, deadline: Instant) -> Start<u32, Failure> {
        Start::new(id).deadline(deadline)
    }

    fn dispatch(&mut self, id: u32) {
        self.dispatched.push(id);
    }

    #[rillet(command)]
    fn receive_receipt(&mut self, id: u32, accepted: bool) {
        if accepted {
            self.succeed_send(&id);
        } else {
            self.fail_send(&id, Failure::Refused);
        }
    }
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[test]
fn immediate_op_succeeds() {
    let store = Store::new().spawn();
    let op = store.submit(2);
    assert!(matches!(*block_on(op.concluded()), OpState::Done { .. }));
    assert_eq!(store.accepted(), vec![2]);
    store.cancel().wait();
}

#[test]
fn immediate_op_fails_with_the_returned_reason() {
    let store = Store::new().spawn();
    let op = store.submit(3);
    assert_eq!(block_on(op.concluded()).failure(), Some(&Rejected::Odd));
    store.cancel().wait();
}

#[test]
fn deferred_op_runs_its_execute_handler() {
    let courier = Courier::new().spawn();
    let op = courier.send(7, far());
    while courier.dispatched().is_empty() {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(courier.dispatched(), vec![7]);
    assert!(op.state().is_pending());
    courier.cancel().wait();
}

#[test]
fn deferred_op_concludes_when_the_outcome_arrives() {
    let courier = Courier::new().spawn();
    let delivered = courier.send(1, far());
    let refused = courier.send(2, far());

    courier.receive_receipt(1, true);
    courier.receive_receipt(2, false);

    assert!(matches!(
        *block_on(delivered.concluded()),
        OpState::Done { .. }
    ));
    assert_eq!(
        block_on(refused.concluded()).failure(),
        Some(&Failure::Refused)
    );
    courier.cancel().wait();
}

#[test]
fn deferred_op_carries_its_deadline() {
    let courier = Courier::new().spawn();
    let deadline = far();
    let op = courier.send(1, deadline);
    // The deadline lands once the service parks the operation.
    let stamped = loop {
        let listener = op.listen();
        if let OpState::Pending {
            deadline: Some(at), ..
        } = *op.state()
        {
            break at;
        }
        block_on(listener);
    };
    assert_eq!(stamped, deadline);
    courier.cancel().wait();
}

#[test]
fn deferred_op_times_out() {
    let courier = Courier::new().spawn();
    let op = courier.send(1, Instant::now() + Duration::from_millis(30));
    assert_eq!(block_on(op.concluded()).failure(), Some(&Failure::TimedOut));
    courier.cancel().wait();
}

#[test]
fn late_receipt_after_expiry_is_a_no_op() {
    let courier = Courier::new().spawn();
    let op = courier.send(1, Instant::now() + Duration::from_millis(30));
    block_on(op.concluded());

    courier.receive_receipt(1, true);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(op.state().failure(), Some(&Failure::TimedOut));
    courier.cancel().wait();
}

#[test]
fn op_sent_to_a_cancelled_service_is_lost() {
    let store = Store::new().spawn();
    store.cancel().wait();
    let op = store.submit(2);
    assert!(matches!(*block_on(op.concluded()), OpState::Lost { .. }));
}
