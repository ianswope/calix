use crate::google::calendar_api;
use crate::store::{EventDraft, Store};
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
/// synced events that are gone on Google's side). Returns the number of
/// calendars synced, for user-facing feedback.
///
/// Blocks on network I/O — call from a background thread. `store` should
/// be a fresh `Store::open()` for that thread, not one shared with the
/// GTK main thread: `Store` wraps a `rusqlite::Connection`, which isn't
/// `Send`.
pub fn sync_account(access_token: &str, store: &Store, account_id: i64) -> Result<usize, String> {
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
                continue;
            }
        };
        let mut synced_ids = Vec::with_capacity(events.len());
        for event in &events {
            let Some((start, all_day)) = event.start.to_local() else {
                continue;
            };
            let Some((end, _)) = event.end.to_local() else {
                continue;
            };
            let draft = EventDraft {
                title: event
                    .summary
                    .clone()
                    .unwrap_or_else(|| "(No title)".to_string()),
                start,
                end,
                all_day,
                location: event.location.clone(),
                notes: notes_with_conference_links(event),
            };
            store
                .upsert_google_event(local_calendar_id, &event.id, &draft)
                .map_err(|e| e.to_string())?;
            synced_ids.push(event.id.clone());
        }
        store
            .prune_google_events(local_calendar_id, &synced_ids, time_min, time_max)
            .map_err(|e| e.to_string())?;
    }

    Ok(calendars.len())
}

fn notes_with_conference_links(event: &calendar_api::EventItem) -> Option<String> {
    let mut lines = event
        .description
        .as_deref()
        .filter(|description| !description.trim().is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(conference_data) = &event.conference_data {
        lines.extend(conference_data.join_links().map(str::to_owned));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}
