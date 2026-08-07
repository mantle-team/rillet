use rillet::CheapClone;

#[derive(Clone, PartialEq, Debug, CheapClone)]
pub struct GaugeView {
    pub level: i64,
}

#[rillet::service(view = GaugeView)]
pub struct Gauge {
    #[rillet(default)]
    level: i64,
}

impl Gauge {
    fn view(&self) -> GaugeView {
        GaugeView { level: self.level }
    }
}

#[rillet::handlers]
impl Gauge {
    #[rillet(direct)]
    fn level_squared(&self) -> i64 {
        self.level * self.level
    }

    #[rillet(direct)]
    fn level_plus(&self, offset: i64) -> i64 {
        self.level + offset
    }

    #[rillet(direct_mut)]
    fn set_level(&mut self, level: i64) -> i64 {
        self.level = level;
        self.level
    }
}

#[test]
fn direct_methods_read_synchronously() {
    let gauge = Gauge::new().spawn();
    assert_eq!(gauge.level_squared(), 0);
    gauge.cancel();
}

#[test]
fn direct_mut_returns_a_value_and_mutates() {
    let gauge = Gauge::new().spawn();
    assert_eq!(gauge.set_level(-4), -4);
    assert_eq!(gauge.level_squared(), 16);
    gauge.cancel();
}

#[test]
fn direct_mut_republishes_the_view_before_returning() {
    let gauge = Gauge::new().spawn();
    let mut watch = gauge.watch_view();

    gauge.set_level(3);

    // The publish happens inside the direct_mut call itself, so the view is
    // current the moment it returns, with no waiting on the service loop.
    assert_eq!(gauge.view().level, 3);
    assert_eq!(watch.try_changed().map(|view| view.level), Some(3));
    gauge.cancel();
}

#[test]
fn direct_methods_take_arguments() {
    let gauge = Gauge::new().spawn();
    gauge.set_level(5);
    assert_eq!(gauge.level_plus(3), 8);
    gauge.cancel();
}
