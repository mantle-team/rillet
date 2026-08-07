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

#[derive(Clone, Debug, PartialEq)]
enum PingFailure {
    TimedOut,
}

#[derive(Clone, Debug, PartialEq)]
enum FetchFailure {
    Missing,
    TimedOut,
}

/// A service with two deferred ops of different key and reason types.
#[rillet::service]
struct Gateway {}

#[rillet::handlers]
impl Gateway {
    #[rillet(op(execute = dispatch_ping, timeout = PingFailure::TimedOut))]
    fn ping(id: u32) -> Start<u32, PingFailure> {
        Start::new(id)
    }

    fn dispatch_ping(&mut self, _id: u32) {}

    #[rillet(op(execute = dispatch_fetch, timeout = FetchFailure::TimedOut))]
    fn fetch(path: String, deadline: Instant) -> Start<String, FetchFailure> {
        Start::new(path).deadline(deadline)
    }

    fn dispatch_fetch(&mut self, _path: String) {}

    #[rillet(command)]
    fn receive_pong(&mut self, id: u32) {
        self.succeed_ping(&id);
    }

    #[rillet(command)]
    fn receive_fetched(&mut self, path: String, found: bool) {
        if found {
            self.succeed_fetch(&path);
        } else {
            self.fail_fetch(&path, FetchFailure::Missing);
        }
    }
}

#[derive(Clone, PartialEq, rillet::CheapClone)]
struct LedgerView {
    total: i64,
}

#[derive(Clone, Debug, PartialEq)]
enum LedgerError {
    Overdrawn,
}

/// A view-publishing service whose op mutates viewed state.
#[rillet::service(view = LedgerView)]
struct Ledger {
    #[rillet(default)]
    total: i64,
}

impl Ledger {
    fn view(&self) -> LedgerView {
        LedgerView { total: self.total }
    }
}

#[rillet::handlers]
impl Ledger {
    #[rillet(op)]
    fn add(&mut self, amount: i64) -> Result<(), LedgerError> {
        if self.total + amount < 0 {
            return Err(LedgerError::Overdrawn);
        }
        self.total += amount;
        Ok(())
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

#[test]
fn parked_op_is_lost_when_the_service_shuts_down() {
    let courier = Courier::new().spawn();
    let op = courier.send(1, far());
    courier.cancel().wait();
    assert!(matches!(*op.state(), OpState::Lost { .. }));
}

#[test]
fn resending_a_key_displaces_the_pending_operation() {
    let courier = Courier::new().spawn();
    let first = courier.send(1, far());
    let second = courier.send(1, far());

    assert!(matches!(*block_on(first.concluded()), OpState::Lost { .. }));
    assert!(second.state().is_pending());
    courier.cancel().wait();
}

#[test]
fn receipt_with_no_pending_operation_is_a_no_op() {
    let courier = Courier::new().spawn();
    courier.receive_receipt(9, true);

    // The service still runs ops normally afterwards.
    let op = courier.send(1, far());
    courier.receive_receipt(1, true);
    assert!(matches!(*block_on(op.concluded()), OpState::Done { .. }));
    courier.cancel().wait();
}

#[test]
fn two_ops_conclude_independently() {
    let gateway = Gateway::new().spawn();
    let ping = gateway.ping(1);
    let fetch = gateway.fetch("a".to_string(), far());

    gateway.receive_pong(1);
    assert!(matches!(*block_on(ping.concluded()), OpState::Done { .. }));
    assert!(fetch.state().is_pending());

    gateway.receive_fetched("a".to_string(), false);
    assert_eq!(
        block_on(fetch.concluded()).failure(),
        Some(&FetchFailure::Missing)
    );
    gateway.cancel().wait();
}

#[test]
fn op_without_a_deadline_stays_pending_and_concludes_on_its_outcome() {
    let gateway = Gateway::new().spawn();
    let op = gateway.ping(1);
    assert!(matches!(
        *op.state(),
        OpState::Pending { deadline: None, .. }
    ));

    gateway.receive_pong(1);
    assert!(matches!(*block_on(op.concluded()), OpState::Done { .. }));
    gateway.cancel().wait();
}

#[test]
fn op_execution_publishes_the_view() {
    let ledger = Ledger::new().spawn();
    let mut changes = ledger.watch_view();

    let op = ledger.add(5);
    assert_eq!(block_on(changes.changed()).total, 5);
    assert!(matches!(*block_on(op.concluded()), OpState::Done { .. }));

    let op = ledger.add(-10);
    assert_eq!(
        block_on(op.concluded()).failure(),
        Some(&LedgerError::Overdrawn)
    );
    assert_eq!(ledger.view().total, 5);
    ledger.cancel().wait();
}

#[test]
fn concluded_op_implies_the_view_shows_its_effects() {
    let ledger = Ledger::new().spawn();
    // Spin rather than park: a woken waiter loses the race window by
    // being slow, and the point is to load the view the instant the
    // outcome is visible.
    for i in 1..=1000i64 {
        let op = ledger.add(1);
        while !op.state().is_terminal() {
            std::hint::spin_loop();
        }
        assert_eq!(
            ledger.view().total,
            i,
            "op {i} concluded before its view published"
        );
    }
    ledger.cancel().wait();
}

#[test]
fn command_stats_cover_ops() {
    let store = Store::new().spawn();
    let op = store.submit(2);
    block_on(op.concluded());

    let submitted = store
        .command_stats()
        .find(|stats| stats.name == "submit")
        .expect("op appears in command stats");
    assert_eq!(submitted.total_enqueued, 1);
    store.cancel().wait();
}
