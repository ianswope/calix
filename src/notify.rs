//! Event alerts: the reminder choices offered by the event dialog, which
//! alerts come due in a tick window, and the notification text shown for
//! them. Pure logic — the GTK wiring (the minute tick and
//! `gio::Notification`) lives in `window.rs`.

use crate::store::Event;
use chrono::{DateTime, Local};

/// The reminder choices offered by the event dialog, in picker order: the
/// label shown in the row and the alert's lead time in minutes before the
/// event start (`None` = no alert).
pub const ALERT_CHOICES: [(&str, Option<i64>); 7] = [
    ("No alert", None),
    ("At time of event", Some(0)),
    ("5 minutes before", Some(5)),
    ("10 minutes before", Some(10)),
    ("30 minutes before", Some(30)),
    ("1 hour before", Some(60)),
    ("1 day before", Some(1440)),
];

/// The reminder lead time for a picker selection.
pub fn from_picker_index(index: u32) -> Option<i64> {
    ALERT_CHOICES
        .get(index as usize)
        .and_then(|(_, minutes)| *minutes)
}

/// The picker position for a stored reminder. Only the dialog writes
/// reminders, so every stored value is on the list; anything else (a stray
/// hand-edited row) falls back to "No alert" rather than panicking.
pub fn picker_index(reminder: Option<i64>) -> u32 {
    ALERT_CHOICES
        .iter()
        .position(|(_, minutes)| *minutes == reminder)
        .unwrap_or(0) as u32
}

/// Events whose alert time falls in the half-open window `(after, up_to]`.
/// The minute tick passes contiguous windows, so an alert fires in exactly
/// one of them — even when a suspend/resume makes a tick late.
pub fn due_alerts(events: &[Event], after: DateTime<Local>, up_to: DateTime<Local>) -> Vec<Event> {
    events
        .iter()
        .filter(|event| {
            event.reminder_minutes.is_some_and(|minutes| {
                let alert_at = event.start - chrono::Duration::minutes(minutes);
                after < alert_at && alert_at <= up_to
            })
        })
        .cloned()
        .collect()
}

/// Body text for an event alert, e.g. "Today at 9:05 AM — Suite 210" or
/// "Tomorrow, all day". Starts further out (a 1-day alert can precede a
/// Monday event by a weekend) show the weekday and date instead.
pub fn notification_body(event: &Event, now: DateTime<Local>) -> String {
    let day = match (event.start.date_naive() - now.date_naive()).num_days() {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => event.start.format("%A, %B %-d").to_string(),
    };
    let when = if event.all_day {
        format!("{day}, all day")
    } else {
        // Same 12-hour convention as the drag preview (drag::format_minutes).
        format!("{day} at {}", event.start.format("%-I:%M %p"))
    };
    match &event.location {
        Some(location) => format!("{when} — {location}"),
        None => when,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, hour: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, hour, min, 0).unwrap()
    }

    fn event_at(start: DateTime<Local>, reminder_minutes: Option<i64>) -> Event {
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Local".into(),
            calendar_color: "#3584e4".into(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Dentist".into(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            google_event_id: None,
            icloud_event_id: None,
            account_server_url: None,
            recurrence: None,
            reminder_minutes,
        }
    }

    #[test]
    fn every_alert_choice_round_trips_through_its_picker_index() {
        for (index, (_, minutes)) in ALERT_CHOICES.iter().enumerate() {
            assert_eq!(from_picker_index(index as u32), *minutes);
            assert_eq!(picker_index(*minutes), index as u32);
        }
    }

    #[test]
    fn unlisted_reminder_values_fall_back_to_no_alert() {
        assert_eq!(picker_index(Some(17)), 0);
        assert_eq!(from_picker_index(99), None);
    }

    #[test]
    fn an_alert_fires_when_its_lead_time_enters_the_tick_window() {
        let event = event_at(at(2026, 7, 20, 10, 0), Some(10));
        let due = due_alerts(
            std::slice::from_ref(&event),
            at(2026, 7, 20, 9, 49),
            at(2026, 7, 20, 9, 50),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, event.id);
    }

    #[test]
    fn an_alert_fires_in_exactly_one_of_two_adjacent_windows() {
        // Alert time 10:00 sharp: on the boundary between (9:59, 10:00] and
        // (10:00, 10:01], it belongs to the earlier window only.
        let event = event_at(at(2026, 7, 20, 10, 0), Some(0));
        let earlier = due_alerts(
            std::slice::from_ref(&event),
            at(2026, 7, 20, 9, 59),
            at(2026, 7, 20, 10, 0),
        );
        let later = due_alerts(
            std::slice::from_ref(&event),
            at(2026, 7, 20, 10, 0),
            at(2026, 7, 20, 10, 1),
        );
        assert_eq!(earlier.len(), 1);
        assert!(later.is_empty());
    }

    #[test]
    fn events_without_a_reminder_never_fire() {
        let event = event_at(at(2026, 7, 20, 10, 0), None);
        let due = due_alerts(
            std::slice::from_ref(&event),
            at(2026, 7, 20, 9, 0),
            at(2026, 7, 20, 11, 0),
        );
        assert!(due.is_empty());
    }

    #[test]
    fn an_alert_already_past_at_the_window_start_does_not_fire() {
        let event = event_at(at(2026, 7, 20, 10, 0), Some(10));
        let due = due_alerts(
            std::slice::from_ref(&event),
            at(2026, 7, 20, 9, 51),
            at(2026, 7, 20, 9, 52),
        );
        assert!(due.is_empty());
    }

    #[test]
    fn notification_body_says_today_with_the_start_time() {
        let event = event_at(at(2026, 7, 20, 14, 30), Some(10));
        let body = notification_body(&event, at(2026, 7, 20, 14, 20));
        assert_eq!(body, "Today at 2:30 PM");
    }

    #[test]
    fn notification_body_appends_the_location_when_set() {
        let mut event = event_at(at(2026, 7, 20, 9, 5), Some(5));
        event.location = Some("Suite 210".into());
        let body = notification_body(&event, at(2026, 7, 20, 9, 0));
        assert_eq!(body, "Today at 9:05 AM — Suite 210");
    }

    #[test]
    fn notification_body_for_an_all_day_event_tomorrow_skips_the_time() {
        let mut event = event_at(at(2026, 7, 21, 0, 0), Some(1440));
        event.all_day = true;
        let body = notification_body(&event, at(2026, 7, 20, 0, 0));
        assert_eq!(body, "Tomorrow, all day");
    }

    #[test]
    fn notification_body_beyond_tomorrow_names_the_weekday_and_date() {
        // A 1-day alert for a Monday 9:00 AM event fires Sunday morning —
        // "Tomorrow" would be right, but a hand-moved clock or suspend can
        // stretch the gap past a day, so the fallback must stay readable.
        let event = event_at(at(2026, 7, 23, 9, 0), Some(1440));
        let body = notification_body(&event, at(2026, 7, 20, 9, 0));
        assert_eq!(body, "Thursday, July 23 at 9:00 AM");
    }
}
