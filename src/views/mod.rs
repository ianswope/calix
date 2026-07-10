use crate::store::Event;
use chrono::{DateTime, Local, NaiveDate, NaiveTime};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

pub(crate) mod drag;
mod event_widget;
pub mod month_view;
pub mod week_view;

/// Whether an event's half-open time range includes a calendar date.
pub(crate) fn event_occurs_on_day(event: &Event, day: NaiveDate) -> bool {
    let start = event.start.date_naive();
    let mut end = event.end.date_naive();
    if event.end.time() == NaiveTime::MIN && event.end > event.start {
        end -= chrono::Duration::days(1);
    }
    start <= day && day <= end
}

/// Attach a right-click "New Event" context menu to `widget`. `moment_at`
/// maps the press position (in `widget` coordinates) to the start time the
/// menu offers. Presses that land on event chips/blocks (buttons) are left
/// alone — those may grow a context menu of their own someday.
pub(crate) fn add_new_event_menu(
    widget: &impl IsA<gtk::Widget>,
    moment_at: impl Fn(f64, f64) -> Option<DateTime<Local>> + 'static,
    on_create: Rc<dyn Fn(DateTime<Local>)>,
) {
    let target = widget.clone().upcast::<gtk::Widget>();
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |gesture, _, x, y| {
        if press_hits_button(&target, x, y) {
            return;
        }
        let Some(start) = moment_at(x, y) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        show_new_event_menu(&target, x, y, start, on_create.clone());
    });
    widget.add_controller(gesture);
}

/// Whether the press landed on a button (an event chip/block) rather than
/// empty calendar space.
fn press_hits_button(root: &gtk::Widget, x: f64, y: f64) -> bool {
    let mut widget = root.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(current) = widget {
        if current == *root {
            return false;
        }
        if current.is::<gtk::Button>() {
            return true;
        }
        widget = current.parent();
    }
    false
}

fn show_new_event_menu(
    parent: &gtk::Widget,
    x: f64,
    y: f64,
    start: DateTime<Local>,
    on_create: Rc<dyn Fn(DateTime<Local>)>,
) {
    let popover = gtk::Popover::new();
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.add_css_class("menu");
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    let item = gtk::Button::with_label("New Event");
    item.add_css_class("flat");
    if let Some(label) = item.child().and_downcast::<gtk::Label>() {
        label.set_halign(gtk::Align::Start);
    }
    popover.set_child(Some(&item));

    let weak = popover.downgrade();
    item.connect_clicked(move |_| {
        if let Some(popover) = weak.upgrade() {
            popover.popdown();
        }
        on_create(start);
    });

    // A dismissed popover must be manually unparented or it (and everything
    // its closures captured) lives as long as its parent widget; deferred to
    // idle so it isn't yanked out from under the `closed` emission.
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}
