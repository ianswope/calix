use crate::google::calendar_api;
use crate::store::{EventDraft, Store};
use crate::sync::SyncOutcome;
use chrono::{Duration, Local};

const SYNC_PAST_DAYS: i64 = 90;
const SYNC_FUTURE_DAYS: i64 = 180;

/// Returns a stable Google account identity from the signed-in account's
/// primary calendar. This avoids needing extra OAuth profile scopes.
pub fn account_identity(access_token: &str) -> Result<(String, String), String> {
    let calendars = calendar_api::list_calendars(access_token)?;
    let calendar = calendars
        .iter()
        .find(|calendar| calendar.primary == Some(true))
        .or_else(|| calendars.first())
        .ok_or_else(|| "Google account has no calendars".to_string())?;
    Ok((calendar.id.clone(), calendar.summary.clone()))
}

/// Fetches every Google calendar and its events in a fixed window
/// around now, and upserts them into the local store (removing previously
/// synced events that are gone on Google's side). Returns a [`SyncOutcome`]
/// recording which calendars synced and which failed, for user-facing feedback.
///
/// Blocks on network I/O — call from a background thread. `store` should
/// be a fresh `Store::open()` for that thread, not one shared with the
/// GTK main thread: `Store` wraps a `rusqlite::Connection`, which isn't
/// `Send`.
pub fn sync_account(
    access_token: &str,
    store: &Store,
    account_id: i64,
) -> Result<SyncOutcome, String> {
    let calendars = calendar_api::list_calendars(access_token)?;

    let time_min = Local::now() - Duration::days(SYNC_PAST_DAYS);
    let time_max = Local::now() + Duration::days(SYNC_FUTURE_DAYS);
    let calendar_ids = calendars
        .iter()
        .map(|calendar| calendar.id.clone())
        .collect::<Vec<_>>();
    store
        .prune_google_calendars(account_id, &calendar_ids)
        .map_err(|e| e.to_string())?;

    let mut outcome = SyncOutcome::default();
    for calendar in &calendars {
        let color = calendar
            .background_color
            .clone()
            .unwrap_or_else(|| "#3584e4".to_string());
        let local_calendar_id = store
            .upsert_google_calendar(
                account_id,
                &calendar.id,
                &calendar.summary,
                &color,
                calendar.is_visible(),
            )
            .map_err(|e| e.to_string())?;

        let events = match calendar_api::list_events(access_token, &calendar.id, time_min, time_max)
        {
            Ok(events) => events,
            Err(error) => {
                eprintln!(
                    "calix: failed to sync Google calendar {} ({}): {}",
                    calendar.summary, calendar.id, error
                );
                outcome.record_failure(calendar.summary.clone());
                continue;
            }
        };

        let reconciliation = reconcile_events(&events);
        for skipped in &reconciliation.skipped {
            eprintln!(
                "calix: keeping cached Google event {skipped} on {} — unsupported start/end",
                calendar.summary
            );
        }
        for (event_id, draft) in &reconciliation.upserts {
            store
                .upsert_google_event(local_calendar_id, event_id, draft)
                .map_err(|e| e.to_string())?;
        }
        store
            .prune_google_events(
                local_calendar_id,
                &reconciliation.keep_ids,
                time_min,
                time_max,
            )
            .map_err(|e| e.to_string())?;
        outcome.record_success();
    }

    Ok(outcome)
}

struct Reconciliation<'a> {
    upserts: Vec<(&'a str, EventDraft)>,
    keep_ids: Vec<String>,
    skipped: Vec<&'a str>,
}

/// Splits fetched events into the drafts to upsert and the full set of ids to
/// keep. Every returned event's id is kept — even one we can't turn into a
/// draft — so a transient parse failure never lets pruning delete an event that
/// Google still returned.
fn reconcile_events(events: &[calendar_api::EventItem]) -> Reconciliation<'_> {
    let mut upserts = Vec::new();
    let mut keep_ids = Vec::with_capacity(events.len());
    let mut skipped = Vec::new();
    for event in events {
        keep_ids.push(event.id.clone());
        match event_draft(event) {
            Some(draft) => upserts.push((event.id.as_str(), draft)),
            None => skipped.push(event.id.as_str()),
        }
    }
    Reconciliation {
        upserts,
        keep_ids,
        skipped,
    }
}

fn event_draft(event: &calendar_api::EventItem) -> Option<EventDraft> {
    let (start, all_day) = event.start.to_local()?;
    let (end, _) = event.end.to_local()?;
    Some(EventDraft {
        title: event
            .summary
            .clone()
            .unwrap_or_else(|| "(No title)".to_string()),
        start,
        end,
        all_day,
        location: event.location.clone(),
        notes: notes_with_conference_links(event),
    })
}

fn notes_with_conference_links(event: &calendar_api::EventItem) -> Option<String> {
    let links = event
        .conference_data
        .as_ref()
        .map(|conference| conference.join_links().collect::<Vec<_>>())
        .unwrap_or_default();
    merge_notes(event.description.as_deref(), &links)
}

/// Combines an event's description with its conference join links, skipping any
/// link already present in the description. Editing an event sends these
/// combined notes back as Google's `description`; without the de-dup, the link
/// would be appended again on the next sync and multiply on every cycle.
fn merge_notes(description: Option<&str>, links: &[&str]) -> Option<String> {
    let mut lines: Vec<String> = description
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect();
    for link in links {
        if !lines.iter().any(|line| line.contains(link)) {
            lines.push((*link).to_string());
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::calendar_api::{EventDateTime, EventItem};

    fn timed(date_time: &str) -> EventDateTime {
        EventDateTime {
            date: None,
            date_time: Some(date_time.to_string()),
            time_zone: None,
        }
    }

    fn unparseable() -> EventDateTime {
        EventDateTime {
            date: None,
            date_time: None,
            time_zone: None,
        }
    }

    fn event(id: &str, start: EventDateTime, end: EventDateTime) -> EventItem {
        EventItem {
            id: id.to_string(),
            summary: Some("Event".to_string()),
            location: None,
            description: None,
            status: String::new(),
            event_type: String::new(),
            conference_data: None,
            start,
            end,
        }
    }

    #[test]
    fn reconcile_keeps_the_id_of_an_event_it_cannot_parse() {
        let events = vec![
            event(
                "good",
                timed("2026-01-10T09:00:00Z"),
                timed("2026-01-10T10:00:00Z"),
            ),
            event("bad", unparseable(), unparseable()),
        ];

        let reconciliation = reconcile_events(&events);

        // Both ids survive pruning; only the parseable one is upserted.
        assert!(reconciliation.keep_ids.contains(&"good".to_string()));
        assert!(reconciliation.keep_ids.contains(&"bad".to_string()));
        assert_eq!(reconciliation.upserts.len(), 1);
        assert_eq!(reconciliation.upserts[0].0, "good");
        assert_eq!(reconciliation.skipped, vec!["bad"]);
    }

    #[test]
    fn merge_notes_appends_a_conference_link_once() {
        assert_eq!(
            merge_notes(Some("Agenda"), &["https://meet.google.com/abc-def"]).as_deref(),
            Some("Agenda\nhttps://meet.google.com/abc-def")
        );
    }

    #[test]
    fn merge_notes_does_not_duplicate_a_link_already_in_the_description() {
        // The second sync after an edit wrote the link into the description: the
        // link must not be appended a second time.
        let description = "Agenda\nhttps://meet.google.com/abc-def";
        assert_eq!(
            merge_notes(Some(description), &["https://meet.google.com/abc-def"]).as_deref(),
            Some(description)
        );
    }

    #[test]
    fn merge_notes_is_none_without_description_or_links() {
        assert_eq!(merge_notes(None, &[]), None);
    }
}
