//! A read-only calendar fetched from one public `.ics` or `webcal` link.
//!
//! Parsing reuses the iCalendar reader in [`crate::caldav`]. What a server
//! would expand for CalDAV, this module decides itself: a plain repeat rule is
//! stored and expanded locally, so a yearly holiday keeps showing up. A rule
//! with exclusions or moved instances is written out as individual dates
//! inside the same window the other syncs use, because the local expander
//! does not know `EXDATE`.

use crate::caldav::{self, IcsEvent, IcsProperty};
use crate::recurrence::{self, Frequency};
use crate::store::{self, EventDraft, Store};
use crate::sync::SyncOutcome;
use chrono::{DateTime, Duration, Local, Utc};
use std::collections::HashMap;

/// How far back and forward an excluded or overridden rule is written out.
/// A plain rule is not cut off here; the grid expands it as far as it is asked.
const PAST_DAYS: i64 = 90;
const FUTURE_DAYS: i64 = 180;

/// Shown when a feed does not name a color. The same blue the rest of the
/// calendar uses for an event that has none of its own.
const DEFAULT_COLOR: &str = "#3584e4";

/// What a drag, a cut, or an edit is told when the event came from a feed.
pub const READ_ONLY: &str = "Subscribed calendars are read-only";

/// A feed Calix will not write to.
pub fn is_read_only(provider: Option<&str>) -> bool {
    provider == Some(crate::provider::WEBCAL.key)
}

/// One downloaded calendar.
#[derive(Debug)]
pub struct SubscribedDocument {
    pub name: Option<String>,
    pub color: Option<String>,
    /// False when a component could not be read in full. The caller still
    /// stores what it could, and must not delete events to match this list.
    pub complete: bool,
    pub events: Vec<SubscribedEvent>,
}

/// One event row to upsert. `id` is stable across syncs.
#[derive(Debug)]
pub struct SubscribedEvent {
    pub id: String,
    pub draft: EventDraft,
}

struct Component {
    cancelled: bool,
    recurrence_id: Option<DateTime<Local>>,
    rrule: Option<String>,
    exdates: Vec<DateTime<Local>>,
    exdates_readable: bool,
    draft: EventDraft,
}

/// The link Calix fetches. `webcal://` and `webcals://` are `https://`.
///
/// A username or password in the link is refused: it would be stored in the
/// database next to the events. A secret in the query string is kept, because
/// that is how some published calendars are unlisted.
pub fn feed_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Paste a calendar link".to_string());
    }
    let rewritten = rewrite_webcal_scheme(raw);
    let mut url = url::Url::parse(&rewritten).map_err(|_| {
        "A calendar link has to start with https://, http://, or webcal://".to_string()
    })?;
    match url.scheme() {
        "https" | "http" => {}
        _ => {
            return Err(
                "A calendar link has to start with https://, http://, or webcal://".to_string(),
            );
        }
    }
    if url.host_str().is_none() {
        return Err("That calendar link has no server address".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Remove the username and password from the calendar link".to_string());
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

fn rewrite_webcal_scheme(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return raw.to_string();
    };
    match scheme.to_ascii_lowercase().as_str() {
        "webcal" | "webcals" => format!("https://{rest}"),
        _ => raw.to_string(),
    }
}

/// The name stored for a subscription. What the user typed wins. A blank name
/// uses the name in the file, then the site the link points at.
pub fn display_name(typed: &str, feed_name: Option<&str>, url: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        return typed.to_string();
    }
    if let Some(name) = feed_name.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    fallback_name(url)
}

/// The name used when the user didn't type one and the feed didn't either.
pub fn fallback_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "Subscribed calendar".to_string())
}

/// The span an excluded rule is expanded across, around `now`.
pub fn expansion_window(now: DateTime<Local>) -> (DateTime<Local>, DateTime<Local>) {
    (
        now - Duration::days(PAST_DAYS),
        now + Duration::days(FUTURE_DAYS),
    )
}

/// Downloads `url` and returns the body. The caller checks that it is a calendar.
pub fn fetch_document(url: &str) -> Result<String, String> {
    let response = crate::http::client()?
        .get(url)
        .header("Accept", "text/calendar, text/plain;q=0.9, */*;q=0.1")
        .send()
        .map_err(|error| format!("couldn't download the calendar: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the calendar link answered {status}"));
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("couldn't download the calendar: {error}"))?;
    const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
    if bytes.len() > MAX_FEED_BYTES {
        return Err("that calendar is larger than Calix will download".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads one calendar document. `window` bounds only rules that have to be
/// written out date by date; everything else is kept whole.
pub fn parse_document(
    url: &str,
    ics: &str,
    window_start: DateTime<Local>,
    window_end: DateTime<Local>,
) -> Result<SubscribedDocument, String> {
    if !is_calendar_document(ics) {
        return Err("That link did not return a calendar".to_string());
    }
    let (name, color) = calendar_metadata(ics);
    let (components, unreadable) = read_components(ics);
    let (events, trust) = assemble(url, components, window_start, window_end);
    Ok(SubscribedDocument {
        name,
        color,
        complete: unreadable == 0 && trust,
        events,
    })
}

fn is_calendar_document(ics: &str) -> bool {
    caldav::unfold_ics(ics)
        .iter()
        .any(|line| line.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
}

fn calendar_metadata(ics: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut color = None;
    for line in caldav::unfold_ics(ics) {
        let Some((head, value)) = caldav::split_content_line(&line) else {
            continue;
        };
        let key = head.split(';').next().unwrap_or(head);
        if name.is_none() && key.eq_ignore_ascii_case("X-WR-CALNAME") {
            let text = caldav::unescape_ics_text(value).trim().to_string();
            if !text.is_empty() {
                name = Some(text);
            }
        }
        if color.is_none()
            && (key.eq_ignore_ascii_case("X-APPLE-CALENDAR-COLOR")
                || key.eq_ignore_ascii_case("COLOR"))
        {
            color = normalize_color(value);
        }
    }
    (name, color)
}

fn normalize_color(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{hex}"))
}

fn read_components(ics: &str) -> (Vec<(String, Component)>, usize) {
    let mut unreadable = 0usize;
    let mut anonymous = 0usize;
    let mut components = Vec::new();
    for event in caldav::ics_event_properties(ics) {
        let Some(component) = read_component(&event) else {
            if !caldav::ics_status_is_cancelled(&event) {
                unreadable += 1;
            }
            continue;
        };
        let uid = event
            .props
            .get("UID")
            .map(|property| property.value.trim().to_string())
            .filter(|uid| !uid.is_empty())
            .unwrap_or_else(|| {
                anonymous += 1;
                format!("anonymous-{anonymous}")
            });
        components.push((uid, component));
    }
    (components, unreadable)
}

fn read_component(event: &IcsEvent) -> Option<Component> {
    let recurrence_id = match event.props.get("RECURRENCE-ID") {
        Some(property) => Some(caldav::parse_ics_datetime(property)?.0),
        None => None,
    };
    let (exdates, exdates_readable) = match read_exdates(event) {
        Some(exdates) => (exdates, true),
        None => (Vec::new(), false),
    };
    Some(Component {
        cancelled: caldav::ics_status_is_cancelled(event),
        recurrence_id,
        rrule: event
            .props
            .get("RRULE")
            .map(|property| property.value.clone()),
        exdates,
        exdates_readable,
        draft: draft_from(event)?,
    })
}

fn draft_from(event: &IcsEvent) -> Option<EventDraft> {
    let (start, all_day) = caldav::parse_ics_datetime(event.props.get("DTSTART")?)?;
    let end = caldav::event_end(&event.props, start, all_day)?;
    let title = event
        .props
        .get("SUMMARY")
        .map(|property| property.value.clone())
        .unwrap_or_else(|| "(No title)".to_string());
    Some(EventDraft {
        title,
        start,
        end,
        all_day,
        location: event
            .props
            .get("LOCATION")
            .map(|property| property.value.clone()),
        notes: event
            .props
            .get("DESCRIPTION")
            .map(|property| property.value.clone()),
        recurrence: None,
        reminder_minutes: None,
        additional_reminders: Vec::new(),
        attendees: event.attendees.clone(),
        instance_of: None,
        original_start: None,
        cancelled: false,
    })
}

fn read_exdates(event: &IcsEvent) -> Option<Vec<DateTime<Local>>> {
    let mut instants = Vec::new();
    for property in &event.exdates {
        if property.value.trim().is_empty() {
            return None;
        }
        for piece in property.value.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                return None;
            }
            let (when, _) = caldav::parse_ics_datetime(&IcsProperty {
                value: piece.to_string(),
                tzid: property.tzid.clone(),
            })?;
            instants.push(when);
        }
    }
    Some(instants)
}

fn assemble(
    url: &str,
    components: Vec<(String, Component)>,
    window_start: DateTime<Local>,
    window_end: DateTime<Local>,
) -> (Vec<SubscribedEvent>, bool) {
    let mut order = Vec::new();
    let mut groups: HashMap<String, Vec<Component>> = HashMap::new();
    for (uid, component) in components {
        if !groups.contains_key(&uid) {
            order.push(uid.clone());
        }
        groups.entry(uid).or_default().push(component);
    }
    let mut events = Vec::new();
    let mut trust = true;
    for uid in order {
        let Some(group) = groups.remove(&uid) else {
            continue;
        };
        let (produced, group_trust) = events_for_group(url, &uid, &group, window_start, window_end);
        trust &= group_trust;
        events.extend(produced);
    }
    (events, trust)
}

fn events_for_group(
    url: &str,
    uid: &str,
    group: &[Component],
    window_start: DateTime<Local>,
    window_end: DateTime<Local>,
) -> (Vec<SubscribedEvent>, bool) {
    let Some(master_index) = group
        .iter()
        .position(|component| component.recurrence_id.is_none())
    else {
        return (
            group
                .iter()
                .filter(|component| !component.cancelled)
                .map(|component| one_off(url, uid, component))
                .collect(),
            true,
        );
    };
    let master = &group[master_index];
    let overrides: Vec<&Component> = group
        .iter()
        .filter(|component| component.recurrence_id.is_some())
        .collect();
    if master.cancelled {
        return (
            overrides
                .into_iter()
                .filter(|component| !component.cancelled)
                .map(|component| one_off(url, uid, component))
                .collect(),
            true,
        );
    }
    let Some(freq) = master.rrule.as_deref().and_then(Frequency::from_rrule) else {
        return (concrete_events(url, uid, master, &overrides), true);
    };
    if !master.exdates_readable {
        return (vec![one_off(url, uid, master)], false);
    }
    if master.exdates.is_empty() && overrides.is_empty() {
        return (vec![master_with_rule(url, uid, master, freq)], true);
    }
    (
        materialize(url, uid, master, freq, &overrides, window_start, window_end),
        true,
    )
}

fn concrete_events(
    url: &str,
    uid: &str,
    master: &Component,
    overrides: &[&Component],
) -> Vec<SubscribedEvent> {
    let mut events = Vec::new();
    if !master.cancelled {
        events.push(one_off(url, uid, master));
    }
    events.extend(
        overrides
            .iter()
            .filter(|component| !component.cancelled)
            .map(|component| one_off(url, uid, component)),
    );
    events
}

fn master_with_rule(url: &str, uid: &str, master: &Component, freq: Frequency) -> SubscribedEvent {
    let mut draft = master.draft.clone();
    draft.recurrence = Some(freq);
    SubscribedEvent {
        id: event_id(url, uid, None),
        draft,
    }
}

fn one_off(url: &str, uid: &str, component: &Component) -> SubscribedEvent {
    let stamp = component
        .recurrence_id
        .map(|when| instance_stamp(&when, component.draft.all_day));
    let mut draft = component.draft.clone();
    draft.recurrence = None;
    SubscribedEvent {
        id: event_id(url, uid, stamp.as_deref()),
        draft,
    }
}

fn materialize(
    url: &str,
    uid: &str,
    master: &Component,
    freq: Frequency,
    overrides: &[&Component],
    window_start: DateTime<Local>,
    window_end: DateTime<Local>,
) -> Vec<SubscribedEvent> {
    let occurrences = recurrence::occurrences_in(
        master.draft.start,
        repeat_duration(&master.draft),
        freq,
        window_start,
        window_end,
    );
    let mut events = Vec::new();
    for start in occurrences {
        if master
            .exdates
            .iter()
            .any(|excluded| matches_instance(*excluded, start, master.draft.all_day))
        {
            continue;
        }
        let stamp = instance_stamp(&start, master.draft.all_day);
        if let Some(replacement) = overrides.iter().find(|component| {
            component
                .recurrence_id
                .is_some_and(|when| matches_instance(when, start, master.draft.all_day))
        }) {
            if replacement.cancelled {
                continue;
            }
            let mut draft = replacement.draft.clone();
            draft.recurrence = None;
            events.push(SubscribedEvent {
                id: event_id(url, uid, Some(&stamp)),
                draft,
            });
            continue;
        }
        events.push(SubscribedEvent {
            id: event_id(url, uid, Some(&stamp)),
            draft: moved_draft(&master.draft, start),
        });
    }
    for replacement in overrides {
        if replacement.cancelled {
            continue;
        }
        let Some(original) = replacement.recurrence_id else {
            continue;
        };
        let id = event_id(
            url,
            uid,
            Some(&instance_stamp(&original, master.draft.all_day)),
        );
        if events.iter().any(|event| event.id == id) {
            continue;
        }
        if replacement.draft.start < window_end && replacement.draft.end > window_start {
            let mut draft = replacement.draft.clone();
            draft.recurrence = None;
            events.push(SubscribedEvent { id, draft });
        }
    }
    events
}

fn moved_draft(master: &EventDraft, start: DateTime<Local>) -> EventDraft {
    let mut draft = master.clone();
    draft.recurrence = None;
    if master.all_day {
        let days = (master.end.date_naive() - master.start.date_naive())
            .num_days()
            .max(1);
        draft.start = store::day_start(start.date_naive());
        draft.end = store::day_start(start.date_naive() + Duration::days(days));
    } else {
        let duration = master.end - master.start;
        draft.start = start;
        draft.end = start + duration;
    }
    draft
}

fn repeat_duration(draft: &EventDraft) -> Duration {
    if draft.all_day {
        let days = (draft.end.date_naive() - draft.start.date_naive())
            .num_days()
            .max(1);
        Duration::days(days)
    } else {
        draft.end - draft.start
    }
}

fn matches_instance(
    candidate: DateTime<Local>,
    occurrence: DateTime<Local>,
    all_day: bool,
) -> bool {
    if all_day {
        candidate.date_naive() == occurrence.date_naive()
    } else {
        candidate == occurrence
    }
}

fn event_id(url: &str, uid: &str, stamp: Option<&str>) -> String {
    match stamp {
        Some(stamp) => format!("{url}#{uid}#{stamp}"),
        None => format!("{url}#{uid}"),
    }
}

fn instance_stamp(when: &DateTime<Local>, all_day: bool) -> String {
    if all_day {
        when.format("%Y%m%d").to_string()
    } else {
        when.with_timezone(&Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    }
}

/// Fetches the account's link and replaces its cached events.
pub fn sync_account(store: &Store, account: &store::Account) -> Result<SyncOutcome, String> {
    let url = account
        .server_url
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or("this subscription has no link")?;
    let ics = fetch_document(url)?;
    let (start, end) = expansion_window(Local::now());
    let document = parse_document(url, &ics, start, end)?;
    apply_document(store, account.id, url, &account.display_name, &document)
}

/// Writes `document` onto the account's one calendar and, when the document
/// was read in full, drops events the feed no longer contains.
pub fn apply_document(
    store: &Store,
    account_id: i64,
    url: &str,
    display_name: &str,
    document: &SubscribedDocument,
) -> Result<SyncOutcome, String> {
    let before = store.sync_revision();
    let color = document.color.as_deref().unwrap_or(DEFAULT_COLOR);
    let calendar_id = store
        .sync_batch(|| store.upsert_caldav_calendar(account_id, url, display_name, color, true))
        .map_err(|error| error.to_string())?;
    let ids: Vec<String> = document
        .events
        .iter()
        .map(|event| event.id.clone())
        .collect();
    store
        .sync_batch(|| {
            for event in &document.events {
                store.upsert_subscription_event(calendar_id, &event.id, &event.draft)?;
            }
            if document.complete {
                store.prune_subscription_events(calendar_id, &ids)?;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;

    let mut outcome = SyncOutcome::default();
    if document.complete {
        outcome.record_success();
    } else {
        outcome.record_failure(display_name.to_string());
    }
    outcome.changed = store.sync_revision() != before;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn window(start: NaiveDate, end: NaiveDate) -> (DateTime<Local>, DateTime<Local>) {
        (store::day_start(start), store::day_start(end))
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// Joins trimmed lines with CRLF so a fixture can be indented in the test.
    fn calendar(text: &str) -> String {
        let mut body = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\r\n");
        body.push_str("\r\n");
        body
    }

    #[test]
    fn a_subscribed_account_uses_the_typed_name_then_the_file_then_the_site() {
        assert_eq!(
            display_name(
                "  Team  ",
                Some("US Holidays"),
                "https://example.com/holidays.ics"
            ),
            "Team"
        );
        assert_eq!(
            display_name(
                "  ",
                Some(" US Holidays "),
                "https://example.com/holidays.ics"
            ),
            "US Holidays"
        );
        assert_eq!(
            display_name("", None, "https://example.com/holidays.ics"),
            "example.com"
        );
    }

    #[test]
    fn a_webcal_link_is_fetched_over_https() {
        assert_eq!(
            feed_url("webcal://Example.COM/Holidays.ics").unwrap(),
            "https://example.com/Holidays.ics"
        );
        assert_eq!(
            feed_url("webcals://example.com/holidays.ics").unwrap(),
            "https://example.com/holidays.ics"
        );
    }

    #[test]
    fn an_https_link_keeps_its_query_and_drops_its_fragment() {
        assert_eq!(
            feed_url("https://example.com/cal.ics?id=1#section").unwrap(),
            "https://example.com/cal.ics?id=1"
        );
    }

    #[test]
    fn a_link_that_is_not_a_calendar_address_is_refused() {
        assert!(feed_url("").is_err());
        assert!(feed_url("file:///tmp/holidays.ics").is_err());
        assert!(feed_url("webcal://").is_err());
        assert!(feed_url("https://user:secret@example.com/cal.ics").is_err());
    }

    #[test]
    fn a_page_that_is_not_a_calendar_is_refused() {
        let (start, end) = window(date(2026, 7, 1), date(2026, 7, 4));
        let error = parse_document("https://example.com/nope", "<html>nope</html>", start, end)
            .expect_err("html is not a calendar");
        assert_eq!(error, "That link did not return a calendar");
    }

    #[test]
    fn a_feed_uses_its_calendar_name_and_color() {
        // The first space on the continuation is the fold marker; the second is
        // the space between the words.
        let ics = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:US\r\n  Holidays\r\n\
X-APPLE-CALENDAR-COLOR:#ff00aa\r\n\
BEGIN:VEVENT\r\nUID:new-year\r\nSUMMARY:New Year's Day\r\n\
DTSTART;VALUE=DATE:20200101\r\nDTEND;VALUE=DATE:20200102\r\n\
RRULE:FREQ=YEARLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window(date(2026, 1, 1), date(2026, 1, 2));
        let document = parse_document("https://example.com/holidays.ics", ics, start, end).unwrap();
        assert_eq!(document.name.as_deref(), Some("US Holidays"));
        assert_eq!(document.color.as_deref(), Some("#ff00aa"));
        assert!(document.complete);
        assert_eq!(document.events.len(), 1);
        assert_eq!(document.events[0].draft.recurrence, Some(Frequency::Yearly));
    }

    #[test]
    fn a_yearly_subscription_shows_up_years_after_it_started() {
        let ics = calendar(
            "
            BEGIN:VCALENDAR
            X-WR-CALNAME:US Holidays
            BEGIN:VEVENT
            UID:new-year
            SUMMARY:New Year's Day
            DTSTART;VALUE=DATE:20200101
            DTEND;VALUE=DATE:20200102
            RRULE:FREQ=YEARLY
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let url = "https://example.com/holidays.ics";
        let (start, end) = window(date(2026, 1, 1), date(2026, 1, 2));
        let document = parse_document(url, &ics, start, end).unwrap();
        let store = Store::open_in_memory().unwrap();
        let account_id = store.upsert_webcal_account(url, "US Holidays").unwrap();
        apply_document(&store, account_id, url, "US Holidays", &document).unwrap();

        let events = store
            .events_between(
                store::day_start(date(2026, 1, 1)),
                store::day_start(date(2026, 1, 2)),
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "New Year's Day");
        assert_eq!(events[0].start.date_naive(), date(2026, 1, 1));
        assert!(events[0].all_day);
        assert_eq!(events[0].account_provider.as_deref(), Some("webcal"));
        assert!(is_read_only(events[0].account_provider.as_deref()));
    }

    #[test]
    fn an_excluded_day_of_a_repeating_feed_is_not_stored() {
        let ics = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:standup
            SUMMARY:Standup
            DTSTART;VALUE=DATE:20260701
            DTEND;VALUE=DATE:20260702
            RRULE:FREQ=DAILY
            EXDATE;VALUE=DATE:20260702
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let (start, end) = window(date(2026, 7, 1), date(2026, 7, 4));
        let document = parse_document("https://example.com/team.ics", &ics, start, end).unwrap();
        let days: Vec<NaiveDate> = document
            .events
            .iter()
            .map(|event| event.draft.start.date_naive())
            .collect();
        assert_eq!(days, vec![date(2026, 7, 1), date(2026, 7, 3)]);
        assert!(
            document
                .events
                .iter()
                .all(|event| event.draft.recurrence.is_none())
        );
    }

    #[test]
    fn an_event_that_leaves_the_feed_is_dropped_on_the_next_sync() {
        let url = "https://example.com/team.ics";
        let first = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:keep
            SUMMARY:Keep
            DTSTART;VALUE=DATE:20260709
            DTEND;VALUE=DATE:20260710
            END:VEVENT
            BEGIN:VEVENT
            UID:gone
            SUMMARY:Gone
            DTSTART;VALUE=DATE:20260710
            DTEND;VALUE=DATE:20260711
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let second = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:keep
            SUMMARY:Keep
            DTSTART;VALUE=DATE:20260709
            DTEND;VALUE=DATE:20260710
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let (start, end) = window(date(2026, 7, 1), date(2026, 8, 1));
        let store = Store::open_in_memory().unwrap();
        let account_id = store.upsert_webcal_account(url, "Team").unwrap();
        let again = store.upsert_webcal_account(url, "Team").unwrap();
        assert_eq!(account_id, again);

        let first = parse_document(url, &first, start, end).unwrap();
        apply_document(&store, account_id, url, "Team", &first).unwrap();
        let second = parse_document(url, &second, start, end).unwrap();
        apply_document(&store, account_id, url, "Team", &second).unwrap();

        let titles: Vec<String> = store
            .events_between(start, end)
            .unwrap()
            .into_iter()
            .map(|event| event.title)
            .collect();
        assert_eq!(titles, vec!["Keep".to_string()]);
    }

    #[test]
    fn an_unreadable_event_does_not_drop_what_was_already_stored() {
        let url = "https://example.com/team.ics";
        let good = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:keep
            SUMMARY:Keep
            DTSTART;VALUE=DATE:20260709
            DTEND;VALUE=DATE:20260710
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let broken = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:bad
            SUMMARY:Bad
            DTSTART;VALUE=PERIOD:nonsense
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let (start, end) = window(date(2026, 7, 1), date(2026, 8, 1));
        let store = Store::open_in_memory().unwrap();
        let account_id = store.upsert_webcal_account(url, "Team").unwrap();
        let good = parse_document(url, &good, start, end).unwrap();
        apply_document(&store, account_id, url, "Team", &good).unwrap();
        let broken = parse_document(url, &broken, start, end).unwrap();
        assert!(!broken.complete);
        apply_document(&store, account_id, url, "Team", &broken).unwrap();

        let titles: Vec<String> = store
            .events_between(start, end)
            .unwrap()
            .into_iter()
            .map(|event| event.title)
            .collect();
        assert_eq!(titles, vec!["Keep".to_string()]);
    }

    #[test]
    fn a_feed_without_a_color_uses_the_calendar_blue() {
        let ics = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:one
            SUMMARY:One
            DTSTART;VALUE=DATE:20260709
            DTEND;VALUE=DATE:20260710
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let url = "https://example.com/one.ics";
        let (start, end) = window(date(2026, 7, 1), date(2026, 8, 1));
        let document = parse_document(url, &ics, start, end).unwrap();
        assert!(document.color.is_none());
        let store = Store::open_in_memory().unwrap();
        let account_id = store.upsert_webcal_account(url, "One").unwrap();
        apply_document(&store, account_id, url, "One", &document).unwrap();
        let calendars = store.calendars_for_account(account_id).unwrap();
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].color, "#3584e4");
    }

    #[test]
    fn subscribing_twice_does_not_duplicate_the_event() {
        let ics = calendar(
            "
            BEGIN:VCALENDAR
            BEGIN:VEVENT
            UID:one
            SUMMARY:One
            DTSTART;VALUE=DATE:20260709
            DTEND;VALUE=DATE:20260710
            END:VEVENT
            END:VCALENDAR
            ",
        );
        let url = "https://example.com/one.ics";
        let (start, end) = window(date(2026, 7, 1), date(2026, 8, 1));
        let document = parse_document(url, &ics, start, end).unwrap();
        let store = Store::open_in_memory().unwrap();
        let account_id = store.upsert_webcal_account(url, "One").unwrap();
        apply_document(&store, account_id, url, "One", &document).unwrap();
        apply_document(&store, account_id, url, "One", &document).unwrap();
        assert_eq!(store.events_between(start, end).unwrap().len(), 1);
    }
}
