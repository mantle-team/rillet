use std::sync::Arc;

use rillet::CheapClone;
use rillet::view::{SmolStr, im};

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
