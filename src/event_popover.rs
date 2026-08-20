//! The event inspector: a lightweight popover shown when an event is clicked,
//! with the full edit dialog one button away.
//!
//! Clicking an event used to open the edit dialog directly, which made the
//! common case — glancing at when something is, or who else is on it — cost a
//! modal you then had to dismiss. The popover answers that at a glance and
//! keeps editing available for when it's actually wanted.
//!
//! The wording logic lives here as pure functions; the widget assembly below
//! it is thin enough to verify by looking at it.

use crate::event_dialog::RemoteEvent;
use crate::store::Store;
use crate::store::{Attendee, Event};
use chrono::{Duration, NaiveDate};
use gtk::gdk;
use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration as StdDuration;

/// `Mon, Aug 10, 2026`
const DATE_FORMAT: &str = "%a, %b %-d, %Y";
/// `Mon, Aug 10`
const DATE_NO_YEAR_FORMAT: &str = "%a, %b %-d";
/// `9:05 AM`
const TIME_FORMAT: &str = "%-I:%M %p";

/// The last calendar day an event covers, as a person would name it.
///
/// All-day events are stored with an exclusive end — a single all-day event on
/// the 10th ends at midnight on the 11th — so the stored end date is one past
/// the last day the event is actually on. Reporting it raw would describe every
/// one-day all-day event as spanning two.
pub(crate) fn inclusive_end_date(event: &Event) -> NaiveDate {
    let end = event.end.date_naive();
    if event.all_day && event.end.time() == chrono::NaiveTime::MIN && event.end > event.start {
        end - Duration::days(1)
    } else {
        end
    }
}

/// The "when" line shown at the top of the inspector.
///
/// The year is printed once, at the end, rather than after every date: a range
/// inside one year reads as a range, not as two separate dates that happen to
/// share a suffix.
pub(crate) fn when_text(event: &Event) -> String {
    let start_date = event.start.date_naive();
    let end_date = inclusive_end_date(event);
    match (event.all_day, start_date == end_date) {
        (true, true) => format!("{} · All day", event.start.format(DATE_FORMAT)),
        (true, false) => format!(
            "{} – {} · All day",
            start_date.format(DATE_NO_YEAR_FORMAT),
            end_date.format(DATE_FORMAT)
        ),
        (false, true) => format!(
            "{} · {} – {}",
            event.start.format(DATE_FORMAT),
            event.start.format(TIME_FORMAT),
            event.end.format(TIME_FORMAT)
        ),
        (false, false) => format!(
            "{}, {} – {}, {}, {}",
            event.start.format(DATE_NO_YEAR_FORMAT),
            event.start.format(TIME_FORMAT),
            event.end.format(DATE_NO_YEAR_FORMAT),
            event.end.format(TIME_FORMAT),
            event.end.format("%Y")
        ),
    }
}

/// One attendee as a line in the inspector: the name the provider sent, or
/// their address when it didn't, with the response appended when known.
///
/// Shares [`crate::event_dialog::attendee_status_label`] rather than
/// re-spelling the vocabulary, so the same reply can't read as "Maybe" in one
/// place and "tentative" in the other.
pub(crate) fn attendee_line(attendee: &Attendee) -> String {
    match attendee.status.as_deref() {
        Some(status) => format!(
            "{} · {}",
            attendee.label(),
            crate::event_dialog::attendee_status_label(status)
        ),
        None => attendee.label().to_string(),
    }
}

/// A detail row: dim caption over its value, or nothing when the value is
/// empty. Returning `None` keeps the caller from having to test each field.
fn detail_row(caption: &str, value: Option<&str>) -> Option<gtk::Box> {
    let value = value.map(str::trim).filter(|text| !text.is_empty())?;
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let caption_label = gtk::Label::new(Some(caption));
    caption_label.add_css_class("caption-heading");
    caption_label.add_css_class("dim-label");
    caption_label.set_xalign(0.0);
    let value_label = gtk::Label::new(Some(value));
    value_label.set_xalign(0.0);
    value_label.set_wrap(true);
    value_label.set_max_width_chars(POPOVER_WIDTH_CHARS);
    row.append(&caption_label);
    row.append(&value_label);
    Some(row)
}

/// Wraps prose so a long note can't stretch the popover across the window.
const POPOVER_WIDTH_CHARS: i32 = 34;

/// Shows the inspector for `event`, anchored to the block that was clicked.
///
/// `on_edit` opens the full dialog, after closing the popover — leaving it
/// floating over the dialog it spawned reads as a stuck widget.
///
/// There is deliberately no Delete here yet. Deleting is recurrence-aware —
/// one occurrence or a whole series, with a remote round trip either way — and
/// that logic lives in the dialog. A second, simpler delete path would be the
/// one that quietly gets a series wrong.
pub(crate) fn open(
    anchor: &impl IsA<gtk::Widget>,
    event: &Event,
    on_edit: Rc<dyn Fn(Event)>,
    store: Rc<Store>,
    remote: Option<RemoteEvent>,
    on_changed: Rc<dyn Fn()>,
) {
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Right);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let title = gtk::Label::new(Some(&event.title));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_max_width_chars(POPOVER_WIDTH_CHARS);
    content.append(&title);

    let when = gtk::Label::new(Some(&when_text(event)));
    when.add_css_class("dim-label");
    when.set_xalign(0.0);
    when.set_wrap(true);
    when.set_max_width_chars(POPOVER_WIDTH_CHARS);
    content.append(&when);

    // A colored dot rather than a bare name: the color is how a calendar is
    // recognized on the grid, so it's the faster identifier here too.
    let calendar_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let swatch = gtk::DrawingArea::new();
    swatch.set_content_width(10);
    swatch.set_content_height(10);
    swatch.set_valign(gtk::Align::Center);
    let color = event.calendar_color.clone();
    swatch.set_draw_func(move |_, cr, width, height| {
        let rgba = gdk::RGBA::parse(&color).unwrap_or(gdk::RGBA::BLUE);
        cr.set_source_rgba(
            rgba.red() as f64,
            rgba.green() as f64,
            rgba.blue() as f64,
            rgba.alpha() as f64,
        );
        let radius = (width.min(height) as f64) / 2.0;
        cr.arc(radius, radius, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    });
    let calendar_name = gtk::Label::new(Some(&event.calendar_name));
    calendar_name.add_css_class("dim-label");
    calendar_name.set_xalign(0.0);
    calendar_row.append(&swatch);
    calendar_row.append(&calendar_name);
    content.append(&calendar_row);

    for row in [
        detail_row("Location", event.location.as_deref()),
        detail_row("Notes", event.notes.as_deref()),
    ]
    .into_iter()
    .flatten()
    {
        content.append(&row);
    }

    if !event.attendees.is_empty() {
        let names: Vec<String> = event.attendees.iter().map(attendee_line).collect();
        if let Some(row) = detail_row("Attendees", Some(&names.join("\n"))) {
            content.append(&row);
        }
    }

    if remote
        .as_ref()
        .is_some_and(|remote| remote.can_respond(event))
    {
        let response_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let error = gtk::Label::new(None);
        error.add_css_class("error");
        error.set_wrap(true);
        error.set_visible(false);
        for (label, response) in [
            ("Accept", "accepted"),
            ("Maybe", "tentative"),
            ("Decline", "declined"),
        ] {
            let button = gtk::Button::with_label(label);
            let remote = remote
                .clone()
                .expect("response controls require a remote event");
            let event = event.clone();
            button.connect_clicked(clone!(
                #[strong]
                store,
                #[strong]
                on_changed,
                #[strong]
                error,
                #[weak]
                response_box,
                move |_| {
                    response_box.set_sensitive(false);
                    error.set_visible(false);
                    let event_for_worker = event.clone();
                    let remote_for_worker = remote.clone();
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let result = remote_for_worker.respond(&event_for_worker, response);
                        let _ = tx.send(result);
                    });
                    let event = event.clone();
                    glib::timeout_add_local(
                        StdDuration::from_millis(100),
                        clone!(
                            #[strong]
                            store,
                            #[strong]
                            on_changed,
                            #[strong]
                            error,
                            #[strong]
                            response_box,
                            move || match rx.try_recv() {
                                Ok(Ok(())) => {
                                    let mut attendees = event.attendees.clone();
                                    if let Some(me) = attendees.iter_mut().find(|attendee| {
                                        attendee.is_self
                                            || attendee.email.eq_ignore_ascii_case(
                                                event.account_provider_id.as_deref().unwrap_or(""),
                                            )
                                    }) {
                                        me.status = Some(response.to_string());
                                    }
                                    let _ = store.update_event_attendees(event.id, &attendees);
                                    on_changed();
                                    glib::ControlFlow::Break
                                }
                                Ok(Err(message)) => {
                                    error.set_label(&message);
                                    error.set_visible(true);
                                    response_box.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                                Err(_) => {
                                    error.set_label("The response stopped unexpectedly");
                                    error.set_visible(true);
                                    response_box.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                            }
                        ),
                    );
                }
            ));
            response_box.append(&button);
        }
        content.append(&response_box);
        content.append(&error);
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let edit_button = gtk::Button::with_label("Edit");
    edit_button.add_css_class("suggested-action");
    buttons.append(&edit_button);
    content.append(&buttons);

    popover.set_child(Some(&content));
    popover.set_parent(&anchor.clone().upcast::<gtk::Widget>());

    let popover_for_edit = popover.clone();
    let event_for_edit = event.clone();
    edit_button.connect_clicked(move |_| {
        popover_for_edit.popdown();
        on_edit(event_for_edit.clone());
    });

    // A popover parented to a widget outlives its own dismissal unless it is
    // explicitly unparented, which would leak one per click.
    popover.connect_closed(|popover| popover.unparent());

    popover.popup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Local, TimeZone};

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("an unambiguous local time")
    }

    fn event(start: DateTime<Local>, end: DateTime<Local>, all_day: bool) -> Event {
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Test".to_string(),
            calendar_color: "#3584e4".to_string(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Trip".to_string(),
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

    #[test]
    fn a_timed_event_inside_one_day_shows_the_date_once() {
        let e = event(at(2026, 8, 10, 9, 0), at(2026, 8, 10, 10, 30), false);
        assert_eq!(when_text(&e), "Mon, Aug 10, 2026 · 9:00 AM – 10:30 AM");
    }

    #[test]
    fn a_timed_event_crossing_midnight_names_both_days() {
        let e = event(at(2026, 8, 10, 21, 0), at(2026, 8, 11, 1, 0), false);
        assert_eq!(
            when_text(&e),
            "Mon, Aug 10, 9:00 PM – Tue, Aug 11, 1:00 AM, 2026"
        );
    }

    #[test]
    fn a_one_day_all_day_event_does_not_claim_to_span_two() {
        // Stored with an exclusive end: midnight on the 11th is still "the
        // 10th, all day" to a reader.
        let e = event(at(2026, 8, 10, 0, 0), at(2026, 8, 11, 0, 0), true);
        assert_eq!(when_text(&e), "Mon, Aug 10, 2026 · All day");
    }

    #[test]
    fn a_multi_day_all_day_event_ends_on_its_last_real_day() {
        // The 10th through the 12th inclusive, stored as ending at midnight on
        // the 13th.
        let e = event(at(2026, 8, 10, 0, 0), at(2026, 8, 13, 0, 0), true);
        assert_eq!(when_text(&e), "Mon, Aug 10 – Wed, Aug 12, 2026 · All day");
    }

    fn attendee(name: Option<&str>, email: &str, status: Option<&str>) -> Attendee {
        Attendee {
            email: email.to_string(),
            name: name.map(str::to_string),
            status: status.map(str::to_string),
            is_self: false,
        }
    }

    #[test]
    fn an_attendee_shows_their_name_and_reply() {
        assert_eq!(
            attendee_line(&attendee(
                Some("Dana"),
                "dana@example.com",
                Some("accepted")
            )),
            "Dana · Accepted"
        );
    }

    #[test]
    fn an_attendee_without_a_name_falls_back_to_their_address() {
        assert_eq!(
            attendee_line(&attendee(None, "dana@example.com", Some("tentative"))),
            "dana@example.com · Maybe"
        );
    }

    #[test]
    fn an_attendee_whose_reply_is_unknown_is_listed_without_one() {
        // A provider that said nothing must not read as "No reply", which is
        // an answer the attendee didn't give.
        assert_eq!(
            attendee_line(&attendee(Some("Dana"), "dana@example.com", None)),
            "Dana"
        );
    }

    #[test]
    fn inclusive_end_leaves_a_timed_event_alone() {
        // Only all-day events carry the exclusive-end convention; a timed event
        // ending at midnight really does end then.
        let e = event(at(2026, 8, 10, 22, 0), at(2026, 8, 11, 0, 0), false);
        assert_eq!(
            inclusive_end_date(&e),
            NaiveDate::from_ymd_opt(2026, 8, 11).expect("a real date")
        );
    }

    #[test]
    fn an_all_day_event_that_does_not_end_at_midnight_is_left_alone() {
        // Defensive: a malformed row shouldn't silently lose a day.
        let e = event(at(2026, 8, 10, 0, 0), at(2026, 8, 11, 9, 0), true);
        assert_eq!(
            inclusive_end_date(&e),
            NaiveDate::from_ymd_opt(2026, 8, 11).expect("a real date")
        );
    }
}
