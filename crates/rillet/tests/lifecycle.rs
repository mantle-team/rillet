mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rillet::Event;
use support::{wait_for, wait_on};

#[derive(Clone, Event)]
pub struct Tick {
    pub value: u32,
}

#[rillet::service]
pub struct Counter {
    #[rillet(get, default)]
    count: u32,
}

#[rillet::handlers]
impl Counter {
    #[rillet(command)]
    fn increment(&mut self) {
        self.count += 1;
    }
}

#[rillet::service]
#[rillet(emits = [Tick])]
pub struct Ticker {}

#[rillet::handlers]
impl Ticker {
    #[rillet(command)]
    fn tick(&mut self, value: u32) {
        self.emit_tick(Tick { value });
    }
}

#[rillet::service]
#[rillet(emits = [Tick])]
pub struct Relay {
    ticker: TickerHandle,
}

#[rillet::handlers]
impl Relay {
    #[rillet(from = ticker)]
    fn on_tick(&mut self, event: Tick) {
        self.emit_tick(event);
    }
}

#[rillet::service]
pub struct Audit {
    ticker: TickerHandle,

    #[rillet(get, default)]
    seen: u32,
}

#[rillet::handlers]
impl Audit {
    #[rillet(from = ticker)]
    fn on_tick(&mut self, event: Tick) {
        self.seen += event.value;
    }
}

#[rillet::service]
pub struct Blocker {
    entered: Arc<AtomicBool>,
    gate: Arc<AtomicBool>,
}

#[rillet::handlers]
impl Blocker {
    #[rillet(command)]
    fn block(&mut self) {
        self.entered.store(true, Ordering::SeqCst);
        while !self.gate.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// Runs the wait on another thread, panicking after five seconds.
fn assert_stops(what: &str, wait: impl FnOnce() + Send + 'static) {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    std::thread::spawn(move || {
        wait();
        flag.store(true, Ordering::SeqCst);
    });
    wait_for(what, || done.load(Ordering::SeqCst));
}

#[test]
fn command_service_stops_when_its_handles_drop() {
    let counter = Counter::new().spawn();
    counter.increment();

    let completion = counter.task_completion();
    drop(counter);

    assert_stops("counter to stop after its handles drop", move || {
        completion.wait()
    });
}

#[test]
fn subscribed_relay_outlives_its_handles_then_stops_unsubscribed() {
    let ticker = Ticker::new().spawn();
    let relay = Relay::new(ticker.clone()).spawn();
    let mut ticks = relay.on_tick();

    let completion = relay.task_completion();
    drop(relay);

    ticker.tick(5);
    assert_eq!(
        wait_on("relayed tick", ticks.next()).map(|t| t.value),
        Some(5)
    );

    drop(ticks);
    ticker.tick(6);

    assert_stops("relay to stop after its last subscriber drops", move || {
        completion.wait()
    });
    ticker.cancel();
}

#[test]
fn cancellation_is_delivered_while_a_handler_holds_the_state_lock() {
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(AtomicBool::new(false));
    let blocker = Blocker::new(entered.clone(), gate.clone()).spawn();

    blocker.block();
    wait_for("the handler to hold the state lock", || {
        entered.load(Ordering::SeqCst)
    });

    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let handle = blocker.clone();
    let waiter = std::thread::spawn(move || {
        let completion = handle.cancel();
        flag.store(true, Ordering::SeqCst);
        completion.wait();
    });

    wait_for("cancel to return while the handler is blocked", || {
        cancelled.load(Ordering::SeqCst)
    });

    gate.store(true, Ordering::SeqCst);
    waiter.join().unwrap();
}

#[test]
fn concurrent_waiters_all_block_until_tasks_finish() {
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(AtomicBool::new(false));
    let blocker = Blocker::new(entered.clone(), gate.clone()).spawn();

    blocker.block();
    wait_for("the handler to hold the state lock", || {
        entered.load(Ordering::SeqCst)
    });

    let first = blocker.cancel();
    let second = blocker.task_completion();
    let flags = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let waiters: Vec<_> = [first, second]
        .into_iter()
        .zip(flags.clone())
        .map(|(completion, flag)| {
            std::thread::spawn(move || {
                completion.wait();
                flag.store(true, Ordering::SeqCst);
            })
        })
        .collect();

    // With the handler still gated, neither waiter may return.
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(flags.iter().all(|flag| !flag.load(Ordering::SeqCst)));

    gate.store(true, Ordering::SeqCst);
    for waiter in waiters {
        waiter.join().unwrap();
    }
    assert!(flags.iter().all(|flag| flag.load(Ordering::SeqCst)));
}

#[test]
fn dropped_joined_completion_does_not_abort_services() {
    let first = Counter::new().spawn();
    let second = Counter::new().spawn();

    drop(rillet::runtime::TaskCompletion::join([
        first.task_completion(),
        second.task_completion(),
    ]));

    first.increment();
    second.increment();
    wait_for("both services to keep processing", || {
        first.count() == 1 && second.count() == 1
    });

    first.cancel();
    second.cancel();
}

#[test]
fn unobserved_event_handler_service_stops_when_its_handles_drop() {
    let ticker = Ticker::new().spawn();
    let audit = Audit::new(ticker.clone()).spawn();

    let completion = audit.task_completion();
    drop(audit);

    assert_stops("audit to stop after its handles drop", move || {
        completion.wait()
    });
    ticker.cancel();
}
