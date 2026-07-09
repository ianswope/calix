use crate::google::calendar_api;
use crate::store::{EventDraft, Store};
use chrono::{Duration, Local};

const SYNC_PAST_DAYS: i64 = 90;
const SYNC_FUTURE_DAYS: i64 = 180;

/// Fetches every visible Google calendar and its events in a fixed window
/// around now, and upserts them into the local store (removing previously
/// synced events that are gone on Google's side). Returns the number of
/// calendars synced, for user-facing feedback.
///
/// Blocks on network I/O — call from a background thread. `store` should
/// be a fresh `Store::open()` for that thread, not one shared with the
/// GTK main thread: `Store` wraps a `rusqlite::Connection`, which isn't
/// `Send`.
pub fn sync(access_token: &str, store: &Store) -> Result<usize, String> {
    let calendars: Vec<_> = calendar_api::list_calendars(access_token)?
        .into_iter()
        .filter(|c| c.is_visible())
        .collect();

    let time_min = Local::now() - Duration::days(SYNC_PAST_DAYS);
    let time_max = Local::now() + Duration::days(SYNC_FUTURE_DAYS);

    for calendar in &calendars {
        let color = calendar.background_color.clone().unwrap_or_else(|| "#3584e4".to_string());
        let local_calendar_id = store
            .upsert_google_calendar(&calendar.id, &calendar.summary, &color)
            .map_err(|e| e.to_string())?;

        let events = calendar_api::list_events(access_token, &calendar.id, time_min, time_max)?;
        let mut synced_ids = Vec::with_capacity(events.len());
        for event in &events {
            let Some((start, all_day)) = event.start.to_local() else { continue };
            let Some((end, _)) = event.end.to_local() else { continue };
            let draft = EventDraft {
                title: event.summary.clone().unwrap_or_else(|| "(No title)".to_string()),
                start,
                end,
                all_day,
                location: event.location.clone(),
                notes: event.description.clone(),
            };
            store
                .upsert_google_event(local_calendar_id, &event.id, &draft)
                .map_err(|e| e.to_string())?;
            synced_ids.push(event.id.clone());
        }
        store
            .prune_google_events(local_calendar_id, &synced_ids)
            .map_err(|e| e.to_string())?;
    }

    Ok(calendars.len())
}
