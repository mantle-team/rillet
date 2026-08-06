mod support;

use std::sync::Arc;

use futures_lite::future::block_on;
use rillet::CheapClone;
use rillet::view::{SmolStr, im};
use support::wait_for;

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

    let view = block_on(watch.changed());
    assert_eq!(view.value, 1);
    assert_eq!(view.doubled, 2);
    assert_eq!(counter.view().value, 1);
    counter.cancel();
}

#[test]
fn unchanged_view_is_not_republished() {
    let counter = Counter::new().spawn();
    let mut watch = counter.watch_view();

    counter.set(5);
    let first = block_on(watch.changed());
    assert_eq!(first.value, 5);

    // Setting the same value mutates nothing observable; the follow-up set
    // proves nothing was published in between.
    counter.set(5);
    counter.set(6);
    let second = block_on(watch.changed());
    assert_eq!(second.value, 6);
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

#[derive(Clone, PartialEq, CheapClone)]
pub struct RosterView {
    pub names: im::Vector<SmolStr>,
    pub motd: Option<Arc<str>>,
}

#[test]
fn blessed_types_compose_into_views() {
    let slot = rillet::view::ViewSlot::new(RosterView {
        names: im::vector![SmolStr::new("ada"), SmolStr::new("grace")],
        motd: None,
    });

    let mut next = (*slot.load()).clone();
    next.names.push_back(SmolStr::new("edsger"));
    assert!(slot.publish(next));
    assert_eq!(slot.load().names.len(), 3);
}

#[test]
fn view_handle_narrows_to_reads_and_watching() {
    let counter = Counter::new().spawn();
    let reader: CounterViewHandle = counter.clone().into();
    let mut watch: rillet::view::ViewWatcher<CounterView> = reader.watch_view();

    counter.increment();
    block_on(watch.changed());

    let view: Arc<CounterView> = reader.view();
    assert_eq!(view.value, 1);
    counter.cancel();
}
