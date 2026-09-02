//! The agenda as JSON, for a status-bar widget to draw.
//!
//! Calix already holds the events a bar widget wants to show, synced and
//! recurrence-expanded, so this is the seam that lets one read them without a
//! second sign-in of its own: `calix --agenda FROM THROUGH` prints the days in
//! that range, `calix --calendars` prints the calendar names. Both run before
//! GTK is touched, alongside `--version`, and both read through
//! [`Store::open_read_only`].
//!
//! The row shape is not ours. It matches what the Omarchy meetings widget
//! already parsed out of `gcalcli agenda --tsv`, down to the all-day end date
//! being exclusive, so the widget keeps its own arithmetic and swaps only where
//! the rows come from. Fields Calix has no answer for — a web link for the
//! event, an organizer — are left out rather than sent empty; the reader
//! already treats a missing column as blank.

use crate::date_util;
use crate::store::{Attendee, Event, Store};
use chrono::{Duration, Local, NaiveDate};
use serde::Serialize;

/// One appointment, in the shape the widget reads.
#[derive(Debug, PartialEq, Serialize)]
pub struct Row {
    /// Unique per occurrence, not per stored row: an expanded recurring series
    /// repeats its master's database id, and the widget uses this to tell one
    /// appointment from another.
    pub id: String,
    pub start_date: String,
    /// `HH:MM`, or empty for an all-day event — which is how the reader tells
    /// the two apart.
    pub start_time: String,
    /// Exclusive for an all-day event, as Google and CalDAV both report it.
    pub end_date: String,
    pub end_time: String,
    pub title: String,
    pub calendar: String,
    pub location: String,
    pub description: String,
    pub attendees: Vec<Person>,
    /// The meeting link, when the notes carry one.
    pub conference_uri: String,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub status: String,
}

/// Hosts whose links are a meeting to join rather than something to read.
/// Matched against the host so a mention in prose can't promote itself into
/// the widget's join button.
const MEETING_HOSTS: [&str; 10] = [
    "meet.google.com",
    "zoom.us",
    "teams.microsoft.com",
    "teams.live.com",
    "meet.jit.si",
    "whereby.com",
    "webex.com",
    "chime.aws",
    "gotomeeting.com",
    "bluejeans.com",
];

/// The first joinable meeting link in `notes`.
///
/// Calix's Google sync folds an event's conference join links into its notes
/// (`notes_with_conference_links`), and CalDAV events carry theirs in the
/// description, so the notes are where a link is in either case.
pub fn conference_link(notes: Option<&str>) -> Option<&str> {
    notes?
        .split_whitespace()
        .map(trim_link)
        .find(|token| is_meeting_link(token))
}

/// Strips the brackets, quotes and sentence punctuation a link picks up from
/// the prose it was written into.
fn trim_link(token: &str) -> &str {
    token
        .trim_start_matches(['<', '(', '[', '"', '\''])
        .trim_end_matches(['>', ')', ']', '"', '\'', '.', ',', ';', ':', '!', '?'])
}

fn is_meeting_link(token: &str) -> bool {
    let Some(rest) = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))
    else {
        return false;
    };
    // Host only: everything after the first delimiter is a path, and anything
    // before an `@` is userinfo, which could otherwise spell out a known host
    // while pointing somewhere else entirely.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    MEETING_HOSTS
        .iter()
        .any(|known| host == *known || host.ends_with(&format!(".{known}")))
}

/// What an agenda command line asked for.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// Every day from `from` through `through`, both included.
    Agenda {
        from: NaiveDate,
        through: NaiveDate,
    },
    Calendars,
}

/// The agenda request in `args`, if there is one.
///
/// `None` means this is an ordinary launch and the command line belongs to GTK.
/// `Some(Err(_))` is a request meant for us that can't be answered — reported
/// rather than rounded down to today, because being shown the wrong days is
/// worse than being told the range was unreadable.
pub fn parse_request(args: &[String]) -> Option<Result<Request, String>> {
    let mut rest = args
        .iter()
        .skip(1)
        .skip_while(|arg| *arg != "--agenda" && *arg != "--calendars");
    if rest.next()? == "--calendars" {
        return Some(Ok(Request::Calendars));
    }

    // Everything positional after `--agenda`; a following flag ends the range.
    let dates: Vec<&String> = rest.take_while(|arg| !arg.starts_with('-')).collect();
    let parse = |text: &str| {
        NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map_err(|_| format!("not a date: {text} (expected YYYY-MM-DD)"))
    };

    // No dates at all is the widget's ordinary case: whatever today is.
    let from = match dates.first() {
        Some(text) => match parse(text) {
            Ok(date) => date,
            Err(complaint) => return Some(Err(complaint)),
        },
        None => Local::now().date_naive(),
    };
    let through = match dates.get(1) {
        Some(text) => match parse(text) {
            Ok(date) => date,
            Err(complaint) => return Some(Err(complaint)),
        },
        None => from,
    };
    if through < from {
        return Some(Err(format!(
            "the range ends before it starts: {from} through {through}"
        )));
    }
    Some(Ok(Request::Agenda { from, through }))
}

/// Answers an agenda command line, printing JSON on stdout.
///
/// `None` if this wasn't one, and the process should carry on into the app;
/// otherwise the exit code to leave with. Failures print a machine-readable
/// `{"error": ...}` for the widget to render a state from, and a sentence on
/// stderr for whoever ran it by hand.
pub fn handle_cli(args: &[String]) -> Option<i32> {
    let request = match parse_request(args)? {
        Ok(request) => request,
        Err(complaint) => return Some(fail("bad-request", &complaint)),
    };
    let store = match Store::open_read_only() {
        Ok(store) => store,
        Err(error) => {
            return Some(fail(
                "not-set-up",
                &format!("no calendar to read yet ({error})"),
            ));
        }
    };
    let json = match request {
        Request::Agenda { from, through } => rows(&store, from, through).map(as_json),
        Request::Calendars => calendar_names(&store).map(as_json),
    };
    match json {
        Ok(json) => {
            println!("{json}");
            Some(0)
        }
        Err(error) => Some(fail(
            "query-failed",
            &format!("could not read the calendar ({error})"),
        )),
    }
}

fn as_json<T: Serialize>(value: T) -> String {
    serde_json::to_string(&value).expect("owned strings and dates always serialize")
}

fn fail(code: &str, message: &str) -> i32 {
    eprintln!("calix: {message}");
    println!("{}", serde_json::json!({ "error": code }));
    1
}

/// One event as a widget row.
pub fn row_for(event: &Event) -> Row {
    let (start_time, end_time, end_date) = if event.all_day {
        // No times at all is what marks an all-day event to the reader, and the
        // end date it wants is exclusive — one past the last day covered,
        // whether the stored span ended at the next midnight or inside its own
        // last day.
        (
            String::new(),
            String::new(),
            date_util::last_covered_day(event.start, event.end) + Duration::days(1),
        )
    } else {
        (
            event.start.format("%H:%M").to_string(),
            event.end.format("%H:%M").to_string(),
            event.end.date_naive(),
        )
    };

    Row {
        // A stored id repeats across an expanded series, so the occurrence's
        // own start is what makes this one identifiable.
        id: format!("{}-{}", event.id, event.start.timestamp()),
        start_date: event.start.date_naive().to_string(),
        start_time,
        end_date: end_date.to_string(),
        end_time,
        title: event.title.clone(),
        calendar: event.calendar_name.clone(),
        location: event.location.clone().unwrap_or_default(),
        description: event.notes.clone().unwrap_or_default(),
        attendees: event.attendees.iter().map(person_for).collect(),
        conference_uri: conference_link(event.notes.as_deref())
            .unwrap_or_default()
            .to_string(),
    }
}

fn person_for(attendee: &Attendee) -> Person {
    Person {
        name: attendee.label().to_string(),
        email: attendee.email.clone(),
        status: attendee.status.clone().unwrap_or_default(),
    }
}

/// Every event from `from` through `through`, both days included.
pub fn rows(store: &Store, from: NaiveDate, through: NaiveDate) -> rusqlite::Result<Vec<Row>> {
    // `through` is inclusive here — a range someone types by hand reads as two
    // days, not a day and a boundary — so the half-open query runs to the start
    // of the day after it.
    let start = date_util::local_day_start(from);
    let end = date_util::local_day_start(through + Duration::days(1));
    Ok(store
        .events_between(start, end)?
        .iter()
        .map(row_for)
        .collect())
}

/// The calendars the widget can offer to show.
///
/// Only the visible ones: [`Store::events_between`] already drops events from a
/// calendar switched off in Calix, so listing it here would draw a tick-box
/// that can never produce an appointment.
pub fn calendar_names(store: &Store) -> rusqlite::Result<Vec<String>> {
    Ok(store
        .calendar_connections()?
        .into_iter()
        .filter(|calendar| calendar.visible)
        .map(|calendar| calendar.name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Attendee, EventDraft};
    use chrono::{DateTime, Local, TimeZone};

    fn at(y: i32, m: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, day, hour, minute, 0)
            .single()
            .expect("an unambiguous local time")
    }

    fn draft(title: &str, start: DateTime<Local>, end: DateTime<Local>) -> EventDraft {
        EventDraft {
            title: title.to_string(),
            start,
            end,
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        }
    }

    /// The event a store gives back after storing `draft` — a round trip, so
    /// the rows under test are built from exactly what a real read produces.
    ///
    /// Attendees go in through their own setter: `create_event` doesn't write
    /// the column, and the sync paths that do use `update_event_attendees`.
    fn stored(draft: &EventDraft) -> Event {
        let store = Store::open_in_memory().expect("an in-memory database");
        let id = store.create_event(1, draft).expect("the event to store");
        if !draft.attendees.is_empty() {
            store
                .update_event_attendees(id, &draft.attendees)
                .expect("the attendees to store");
        }
        store
            .events_between(at(2000, 1, 1, 0, 0), at(2100, 1, 1, 0, 0))
            .expect("the range to query")
            .pop()
            .expect("the stored event")
    }

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("calix")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("a real date")
    }

    #[test]
    fn an_ordinary_launch_is_not_an_agenda_request() {
        assert_eq!(parse_request(&argv(&[])), None);
        assert_eq!(parse_request(&argv(&["2026-08-31"])), None);
        assert_eq!(parse_request(&argv(&["--version"])), None);
    }

    #[test]
    fn a_bare_agenda_flag_asks_for_today() {
        let today = Local::now().date_naive();
        assert_eq!(
            parse_request(&argv(&["--agenda"])),
            Some(Ok(Request::Agenda {
                from: today,
                through: today
            }))
        );
    }

    #[test]
    fn one_date_asks_for_that_single_day() {
        assert_eq!(
            parse_request(&argv(&["--agenda", "2026-08-31"])),
            Some(Ok(Request::Agenda {
                from: day(2026, 8, 31),
                through: day(2026, 8, 31)
            }))
        );
    }

    #[test]
    fn two_dates_ask_for_an_inclusive_range() {
        assert_eq!(
            parse_request(&argv(&["--agenda", "2026-08-31", "2026-09-06"])),
            Some(Ok(Request::Agenda {
                from: day(2026, 8, 31),
                through: day(2026, 9, 6)
            }))
        );
    }

    #[test]
    fn an_unreadable_date_is_reported() {
        let complaint = parse_request(&argv(&["--agenda", "next tuesday"]))
            .expect("an agenda request")
            .expect_err("an unreadable date");
        assert!(complaint.contains("next tuesday"), "got {complaint:?}");
    }

    #[test]
    fn a_backwards_range_is_reported_rather_than_answered_empty() {
        assert!(
            parse_request(&argv(&["--agenda", "2026-09-06", "2026-08-31"]))
                .expect("an agenda request")
                .is_err()
        );
    }

    #[test]
    fn the_calendars_flag_asks_for_the_calendar_list() {
        assert_eq!(
            parse_request(&argv(&["--calendars"])),
            Some(Ok(Request::Calendars))
        );
    }

    #[test]
    fn a_timed_event_reports_clock_times() {
        let row = row_for(&stored(&draft(
            "Standup",
            at(2026, 8, 31, 9, 30),
            at(2026, 8, 31, 10, 0),
        )));

        assert_eq!(row.title, "Standup");
        assert_eq!(row.calendar, "Local");
        assert_eq!(row.start_date, "2026-08-31");
        assert_eq!(row.start_time, "09:30");
        assert_eq!(row.end_date, "2026-08-31");
        assert_eq!(row.end_time, "10:00");
    }

    #[test]
    fn an_event_running_past_midnight_keeps_its_own_end_date() {
        let row = row_for(&stored(&draft(
            "Long call",
            at(2026, 8, 31, 23, 0),
            at(2026, 9, 1, 0, 30),
        )));

        assert_eq!(row.end_date, "2026-09-01");
        assert_eq!(row.end_time, "00:30");
    }

    #[test]
    fn a_one_day_all_day_event_ends_on_the_next_date() {
        // Stored midnight to midnight, and reported the way Google does it:
        // the end date is the day after the one it covers.
        let mut event = draft("Holiday", at(2026, 8, 31, 0, 0), at(2026, 9, 1, 0, 0));
        event.all_day = true;
        let row = row_for(&stored(&event));

        assert_eq!(row.start_date, "2026-08-31");
        assert_eq!(row.end_date, "2026-09-01");
        assert_eq!(row.start_time, "");
        assert_eq!(row.end_time, "");
    }

    #[test]
    fn a_multi_day_all_day_event_keeps_its_span() {
        let mut event = draft("Conference", at(2026, 8, 31, 0, 0), at(2026, 9, 3, 0, 0));
        event.all_day = true;
        let row = row_for(&stored(&event));

        assert_eq!(row.start_date, "2026-08-31");
        assert_eq!(row.end_date, "2026-09-03");
    }

    #[test]
    fn an_all_day_event_stored_without_an_exclusive_end_still_ends_a_day_later() {
        // A local all-day event whose end sits inside its own last day: the
        // exclusive end the widget needs is still the day after that.
        let mut event = draft("Day off", at(2026, 8, 31, 0, 0), at(2026, 8, 31, 17, 0));
        event.all_day = true;
        let row = row_for(&stored(&event));

        assert_eq!(row.end_date, "2026-09-01");
    }

    #[test]
    fn attendees_travel_with_the_row() {
        let mut event = draft("Review", at(2026, 8, 31, 9, 0), at(2026, 8, 31, 10, 0));
        event.attendees = vec![
            Attendee {
                email: "sam@example.com".to_string(),
                name: Some("Sam Okafor".to_string()),
                status: Some("accepted".to_string()),
                is_self: false,
            },
            Attendee {
                email: "nameless@example.com".to_string(),
                name: None,
                status: None,
                is_self: false,
            },
        ];
        let row = row_for(&stored(&event));

        assert_eq!(
            row.attendees,
            vec![
                Person {
                    name: "Sam Okafor".to_string(),
                    email: "sam@example.com".to_string(),
                    status: "accepted".to_string(),
                },
                // No display name from the provider, so the email stands in —
                // a blank line in the attendee list says nothing.
                Person {
                    name: "nameless@example.com".to_string(),
                    email: "nameless@example.com".to_string(),
                    status: String::new(),
                },
            ]
        );
    }

    #[test]
    fn a_meeting_link_is_lifted_out_of_the_notes() {
        assert_eq!(
            conference_link(Some("Agenda inside\nhttps://meet.google.com/abc-defg-hij")),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(
            conference_link(Some("https://example.zoom.us/j/123456?pwd=x")),
            Some("https://example.zoom.us/j/123456?pwd=x")
        );
    }

    #[test]
    fn an_ordinary_link_is_not_mistaken_for_a_meeting() {
        assert_eq!(
            conference_link(Some("Notes: https://example.com/agenda.pdf")),
            None
        );
        assert_eq!(conference_link(Some("No links at all")), None);
        assert_eq!(conference_link(None), None);
    }

    #[test]
    fn a_link_wrapped_in_punctuation_comes_out_clean() {
        assert_eq!(
            conference_link(Some("Join <https://meet.google.com/abc-defg-hij>.")),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }

    #[test]
    fn a_meeting_link_reaches_the_row() {
        let mut event = draft("Sync", at(2026, 8, 31, 9, 0), at(2026, 8, 31, 10, 0));
        event.notes = Some("https://meet.google.com/abc-defg-hij".to_string());
        let row = row_for(&stored(&event));

        assert_eq!(row.conference_uri, "https://meet.google.com/abc-defg-hij");
        // The notes stay whole: the link is part of what the event says.
        assert_eq!(row.description, "https://meet.google.com/abc-defg-hij");
    }

    #[test]
    fn rows_cover_both_ends_of_the_range() {
        let store = Store::open_in_memory().expect("an in-memory database");
        for (title, day) in [("First", 31), ("Middle", 1), ("Last", 2)] {
            let month = if day == 31 { 8 } else { 9 };
            store
                .create_event(
                    1,
                    &draft(
                        title,
                        at(2026, month, day, 9, 0),
                        at(2026, month, day, 10, 0),
                    ),
                )
                .expect("the event to store");
        }
        // A day outside the range, to prove the range is doing something.
        store
            .create_event(
                1,
                &draft("Later", at(2026, 9, 3, 9, 0), at(2026, 9, 3, 10, 0)),
            )
            .expect("the event to store");

        let listed = rows(
            &store,
            NaiveDate::from_ymd_opt(2026, 8, 31).expect("a real date"),
            NaiveDate::from_ymd_opt(2026, 9, 2).expect("a real date"),
        )
        .expect("the rows to build");

        assert_eq!(
            listed
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["First", "Middle", "Last"]
        );
    }

    #[test]
    fn occurrences_of_one_series_get_distinct_ids() {
        use crate::recurrence::Frequency;

        let store = Store::open_in_memory().expect("an in-memory database");
        let mut event = draft("Standup", at(2026, 8, 31, 9, 0), at(2026, 8, 31, 9, 15));
        event.recurrence = Some(Frequency::Daily);
        store.create_event(1, &event).expect("the event to store");

        let listed = rows(
            &store,
            NaiveDate::from_ymd_opt(2026, 8, 31).expect("a real date"),
            NaiveDate::from_ymd_opt(2026, 9, 2).expect("a real date"),
        )
        .expect("the rows to build");

        let ids: Vec<&str> = listed.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert!(
            ids.iter().collect::<std::collections::HashSet<_>>().len() == 3,
            "each occurrence needs its own id, got {ids:?}"
        );
    }

    #[test]
    fn calendar_names_skip_the_ones_switched_off() {
        let store = Store::open_in_memory().expect("an in-memory database");
        assert_eq!(calendar_names(&store).expect("the names"), ["Local"]);

        store
            .set_calendar_visible(1, false)
            .expect("the calendar to hide");
        assert!(calendar_names(&store).expect("the names").is_empty());
    }
}
