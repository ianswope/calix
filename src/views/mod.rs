use crate::store::Event;
use chrono::{DateTime, Local, NaiveDate, NaiveTime};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

pub(crate) mod drag;
mod event_widget;
pub mod month_view;
pub mod week_view;
pub mod year_view;

/// Opens the new-event dialog at `start`. The second argument is the end time
/// when the caller knows one — a create-drag draws an explicit span — and
/// `None` when it should fall back to the default duration.
pub(crate) type CreateFn = Rc<dyn Fn(DateTime<Local>, Option<DateTime<Local>>)>;

/// Opens the inspector for an event. The widget is the chip or block that was
/// clicked, which the popover anchors itself to — a popover with no anchor has
/// nowhere meaningful to point.
pub(crate) type EditFn = Rc<dyn Fn(Event, gtk::Widget)>;

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
    on_create: CreateFn,
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
pub(crate) fn press_hits_button(root: &gtk::Widget, x: f64, y: f64) -> bool {
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
    on_create: CreateFn,
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
        on_create(start, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Event;
    use chrono::{Duration, TimeZone};

    fn event_on(
        start: chrono::DateTime<Local>,
        end: chrono::DateTime<Local>,
        all_day: bool,
    ) -> Event {
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Local".into(),
            calendar_color: "#3584e4".into(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Event".into(),
            start,
            end,
            all_day,
            location: None,
            notes: None,
            google_event_id: None,
            icloud_event_id: None,
            account_server_url: None,
            attendees: Vec::new(),
            recurrence: None,
            reminder_minutes: None,
        }
    }

    fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn a_timed_event_occurs_on_its_start_day() {
        let start = Local.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap();
        let event = event_on(start, start + Duration::hours(1), false);
        assert!(event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
        ));
        assert!(!event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
        ));
    }

    #[test]
    fn an_all_day_event_does_not_include_its_exclusive_end_date() {
        // All-day on the 21st is stored as [21st 00:00, 22nd 00:00).
        let event = event_on(at(2026, 8, 21), at(2026, 8, 22), true);
        assert!(event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
        ));
        assert!(!event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
        ));
    }

    #[test]
    fn a_multi_day_all_day_event_covers_each_inclusive_day() {
        let event = event_on(at(2026, 8, 21), at(2026, 8, 24), true);
        assert!(event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
        ));
        assert!(event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 23).unwrap()
        ));
        assert!(!event_occurs_on_day(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 24).unwrap()
        ));
    }
}
