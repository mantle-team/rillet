mod support;

use support::wait_for;

#[rillet::service]
pub struct Ledger {
    pub label: String,

    #[rillet(get, default)]
    entries: Vec<u32>,

    #[rillet(get, default = 100)]
    limit: u32,
}

#[rillet::handlers]
impl Ledger {
    #[rillet(command)]
    fn record(&mut self, value: u32) {
        if value <= self.limit {
            self.entries.push(value);
        }
    }
}

#[test]
fn constructor_takes_non_default_fields_only() {
    let ledger = Ledger::new("cash".into());
    assert_eq!(ledger.label, "cash");
    let handle = ledger.spawn();
    assert_eq!(handle.limit(), 100);
    handle.cancel();
}

#[test]
fn commands_run_serialized_in_send_order() {
    let ledger = Ledger::new("ordered".into()).spawn();
    for value in 0..50 {
        ledger.record(value);
    }
    wait_for("all commands to process", || ledger.entries().len() == 50);
    assert_eq!(ledger.entries(), (0..50).collect::<Vec<_>>());
    ledger.cancel();
}

#[rillet::service(command_capacity = 4)]
pub struct Narrow {
    #[rillet(get, default)]
    done: u32,
}

#[rillet::handlers]
impl Narrow {
    #[rillet(command)]
    fn stall(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    #[rillet(command)]
    fn tick(&mut self) {
        self.done += 1;
    }
}

#[test]
#[should_panic(expected = "command queue full: tick")]
fn overfilling_a_sized_command_queue_panics() {
    let narrow = Narrow::new().spawn();
    // Park the loop in a slow command, then overfill the 4-slot queue.
    narrow.stall();
    std::thread::sleep(std::time::Duration::from_millis(50));
    for _ in 0..5 {
        narrow.tick();
    }
}

#[test]
fn commands_after_cancel_are_dropped_silently() {
    let ledger = Ledger::new("closed".into()).spawn();
    ledger.cancel().wait();
    // The channel is closed; sending must neither panic nor deliver.
    ledger.record(1);
    assert_eq!(ledger.entries().len(), 0);
}
