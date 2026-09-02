//! Event search: a popover with a query field and its results.
//!
//! The matching itself is [`crate::store::Store::search_events`], where it can
//! be tested against a real database. What lives here is the presentation —
//! how many results to show, what each row says, and what happens when one is
//! chosen.

use crate::event_popover::when_text;
use crate::store::{Event, Store};
use gtk::prelude::*;
use std::rc::Rc;

/// How many matches to show at once.
///
/// A calendar with years of history can match hundreds of rows for a common
/// word, and a list that long is not a list anyone reads — it's a wall. The cap
/// is stated in the UI when it bites, so a truncated result set never passes
/// for a complete one.
const RESULT_LIMIT: usize = 50;

/// The one-line description under a result's title.
///
/// Reuses the inspector's phrasing so the same event doesn't describe itself
/// two different ways depending on where it's seen.
pub(crate) fn result_subtitle(event: &Event) -> String {
    let when = when_text(event);
    match event.location.as_deref().map(str::trim) {
        Some(location) if !location.is_empty() => format!("{when} · {location}"),
        _ => when,
    }
}

/// Opens the search popover anchored to `anchor`.
///
/// `on_pick` is handed the chosen event so the caller can navigate to it;
/// search knows how to find things, not where the calendar should go.
pub(crate) fn open(anchor: &impl IsA<gtk::Widget>, store: Rc<Store>, on_pick: Rc<dyn Fn(Event)>) {
    let (popover, entry) = build(store, on_pick);
    popover.set_parent(&anchor.clone().upcast::<gtk::Widget>());
    // A popover parented to a widget outlives its own dismissal unless it is
    // explicitly unparented, which would leak one per search.
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
    entry.grab_focus();
}

/// The search popover and its entry, built but not yet shown. Separate from
/// [`open`] so the tree can be constructed — and checked for what it holds on
/// to — without a window to anchor it in.
pub(crate) fn build(
    store: Rc<Store>,
    on_pick: Rc<dyn Fn(Event)>,
) -> (gtk::Popover, gtk::SearchEntry) {
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_size_request(360, -1);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);

    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search events"));
    content.append(&entry);

    let results = gtk::ListBox::new();
    results.set_selection_mode(gtk::SelectionMode::None);
    results.add_css_class("boxed-list");

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(0)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&results)
        .build();
    scroller.set_visible(false);
    content.append(&scroller);

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_wrap(true);
    status.set_visible(false);
    content.append(&status);

    popover.set_child(Some(&content));

    entry.connect_search_changed({
        let store = store.clone();
        let on_pick = on_pick.clone();
        let results = results.clone();
        let scroller = scroller.clone();
        let status = status.clone();
        // Weak: the entry is inside the popover, so holding the popover from
        // the entry's handler would keep the whole popover alive for good.
        let popover = popover.downgrade();
        move |entry| {
            let Some(popover) = popover.upgrade() else {
                return;
            };
            while let Some(row) = results.first_child() {
                results.remove(&row);
            }
            let query = entry.text();
            let query = query.trim();
            if query.is_empty() {
                scroller.set_visible(false);
                status.set_visible(false);
                return;
            }
            // One extra past the cap, purely to learn whether more exist —
            // claiming "50 results" when there are 900 would be a lie the user
            // can't see through.
            let found = store
                .search_events(query, RESULT_LIMIT + 1)
                .unwrap_or_default();
            let truncated = found.len() > RESULT_LIMIT;
            let shown = &found[..found.len().min(RESULT_LIMIT)];

            for event in shown {
                results.append(&result_row(event, &popover, &on_pick));
            }
            scroller.set_visible(!shown.is_empty());
            match (shown.is_empty(), truncated) {
                (true, _) => {
                    status.set_label("No events match.");
                    status.set_visible(true);
                }
                (false, true) => {
                    status.set_label(&format!(
                        "Showing the first {RESULT_LIMIT} matches — keep typing to narrow them."
                    ));
                    status.set_visible(true);
                }
                (false, false) => status.set_visible(false),
            }
        }
    });

    (popover, entry)
}

fn result_row(event: &Event, popover: &gtk::Popover, on_pick: &Rc<dyn Fn(Event)>) -> gtk::Button {
    let title = gtk::Label::new(Some(&event.title));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let subtitle = gtk::Label::new(Some(&result_subtitle(event)));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("caption");
    subtitle.add_css_class("dim-label");
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    let button = gtk::Button::builder().css_classes(["flat"]).build();
    button.set_child(Some(&text));

    let event = event.clone();
    // Weak for the same reason as the entry's handler: the row is a
    // descendant of the popover it would otherwise pin.
    let popover = popover.downgrade();
    let on_pick = on_pick.clone();
    button.connect_clicked(move |_| {
        if let Some(popover) = popover.upgrade() {
            popover.popdown();
        }
        on_pick(event.clone());
    });
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Local, TimeZone};

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("an unambiguous local time")
    }

    fn event(location: Option<&str>) -> Event {
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Test".to_string(),
            calendar_color: "#3584e4".to_string(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Dentist".to_string(),
            start: at(2026, 8, 10, 9),
            end: at(2026, 8, 10, 10),
            all_day: false,
            location: location.map(str::to_string),
            notes: None,
            google_event_id: None,
            icloud_event_id: None,
            account_server_url: None,
            attendees: Vec::new(),
            recurrence: None,
            reminder_minutes: None,
        }
    }

    #[test]
    fn a_result_shows_when_it_is_and_where() {
        assert_eq!(
            result_subtitle(&event(Some("Main St"))),
            "Mon, Aug 10, 2026 · 9:00 AM – 10:00 AM · Main St"
        );
    }

    #[test]
    fn a_result_without_a_location_does_not_trail_a_separator() {
        assert_eq!(
            result_subtitle(&event(None)),
            "Mon, Aug 10, 2026 · 9:00 AM – 10:00 AM"
        );
    }

    #[test]
    fn a_blank_location_is_treated_as_no_location() {
        // Providers hand back empty strings as readily as nulls.
        assert_eq!(
            result_subtitle(&event(Some("   "))),
            result_subtitle(&event(None))
        );
    }
}
