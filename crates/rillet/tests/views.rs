mod support;

use std::sync::Arc;

use rillet::CheapClone;
use support::{wait_for, wait_on};

#[derive(Clone, PartialEq, Debug, CheapClone)]
pub struct CounterView {
    pub value: u64,
    pub doubled: u64,
}

#[rillet::service(view = CounterView)]
pub struct Counter {
    #[rillet(default)]
    value: u64,
}

impl Counter {
    fn view(&self) -> CounterView {
        CounterView {
            value: self.value,
            doubled: self.value * 2,
        }
    }
}

#[rillet::handlers]
impl Counter {
    #[rillet(command)]
    fn increment(&mut self) {
        self.value += 1;
    }

    #[rillet(command)]
    fn set(&mut self, value: u64) {
        self.value = value;
    }
}

#[test]
fn view_is_seeded_before_any_mutation() {
    let counter = Counter::new().spawn();
    let view = counter.view();
    assert_eq!(view.value, 0);
    assert_eq!(view.doubled, 0);
    counter.cancel();
}

#[test]
fn mutation_publishes_a_coherent_view() {
    let counter = Counter::new().spawn();
    let mut watch = counter.watch_view();

    counter.increment();

    let view = wait_on("view publish", watch.changed());
    assert_eq!(view.value, 1);
    assert_eq!(view.doubled, 2);
    assert_eq!(counter.view().value, 1);
    counter.cancel();
}

#[test]
fn unchanged_view_is_not_republished() {
    let counter = Counter::new().spawn();

    counter.set(5);
    wait_for("first set to process", || counter.view().value == 5);
    let before = counter.view();

    // A republish of an unchanged view would store a fresh Arc; the
    // processed counter proves the duplicate set fully executed.
    counter.set(5);
    wait_for("second set to process", || {
        counter.aggregate_stats().total_processed == 2
    });
    assert!(Arc::ptr_eq(&before, &counter.view()));

    // Later publishes still arrive.
    let mut watch = counter.watch_view();
    counter.set(6);
    let next = wait_on("changed publish", watch.changed());
    assert_eq!(next.value, 6);
    counter.cancel();
}

#[rillet::service]
pub struct Mirror {
    counter: CounterHandle,

    #[rillet(get, default)]
    latest: u64,
}

#[rillet::handlers]
impl Mirror {
    #[rillet(watch = counter)]
    fn on_counter_view(&mut self, view: Arc<CounterView>) {
        self.latest = view.value;
    }
}

#[test]
fn services_watch_other_services_views() {
    let counter = Counter::new().spawn();
    let mirror = Mirror::new(counter.clone()).spawn();

    counter.set(41);
    wait_for("mirror to observe the view", || mirror.latest() == 41);

    mirror.cancel();
    counter.cancel();
}

#[test]
fn view_handle_narrows_to_reads_and_watching() {
    let counter = Counter::new().spawn();
    let reader: CounterViewHandle = counter.clone().into();
    let mut watch: rillet::view::ViewWatcher<CounterView> = reader.watch_view();

    counter.increment();
    wait_on("view publish", watch.changed());

    let view: Arc<CounterView> = reader.view();
    assert_eq!(view.value, 1);
    counter.cancel();
}

#[rillet::service]
pub struct SplitMirror {
    left: CounterHandle,
    right: CounterHandle,

    #[rillet(get, default)]
    left_value: u64,

    #[rillet(get, default)]
    right_value: u64,
}

#[rillet::handlers]
impl SplitMirror {
    #[rillet(watch = left)]
    fn on_left_view(&mut self, view: Arc<CounterView>) {
        self.left_value = view.value;
    }

    #[rillet(watch = right)]
    fn on_right_view(&mut self, view: Arc<CounterView>) {
        self.right_value = view.value;
    }
}

#[test]
fn one_loop_runs_multiple_watch_handlers() {
    let left = Counter::new().spawn();
    let right = Counter::new().spawn();
    let mirror = SplitMirror::new(left.clone(), right.clone()).spawn();

    left.set(11);
    right.set(22);

    wait_for("both watch handlers to run", || {
        mirror.left_value() == 11 && mirror.right_value() == 22
    });
    mirror.cancel();
    right.cancel();
    left.cancel();
}
