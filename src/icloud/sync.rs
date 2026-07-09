use crate::icloud::caldav::{self, Credentials};
use crate::store::Store;
use chrono::{Duration, Local};

const SYNC_PAST_DAYS: i64 = 90;
const SYNC_FUTURE_DAYS: i64 = 180;

pub fn sync_account(
    credentials: &Credentials,
    store: &Store,
    account_id: i64,
) -> Result<usize, String> {
    let calendars = caldav::discover_calendars(credentials)?;
    let time_min = Local::now() - Duration::days(SYNC_PAST_DAYS);
    let time_max = Local::now() + Duration::days(SYNC_FUTURE_DAYS);
    let calendar_ids = calendars
        .iter()
        .map(|calendar| calendar.href.clone())
        .collect::<Vec<_>>();
    store
        .prune_icloud_calendars(account_id, &calendar_ids)
        .map_err(|e| e.to_string())?;

    for calendar in &calendars {
        let local_calendar_id = store
            .upsert_icloud_calendar(
                account_id,
                &calendar.href,
                &calendar.name,
                &calendar.color,
                true,
            )
            .map_err(|e| e.to_string())?;

        let events = match caldav::calendar_events(credentials, &calendar.href, time_min, time_max)
        {
            Ok(events) => events,
            Err(error) => {
                eprintln!(
                    "calix: failed to sync iCloud calendar {} ({}): {}",
                    calendar.name, calendar.href, error
                );
                continue;
            }
        };
        let mut synced_ids = Vec::with_capacity(events.len());
        for event in events {
            store
                .upsert_icloud_event(local_calendar_id, &event.href, &event.draft)
                .map_err(|e| e.to_string())?;
            synced_ids.push(event.href);
        }
        store
            .prune_icloud_events(local_calendar_id, &synced_ids, time_min, time_max)
            .map_err(|e| e.to_string())?;
    }

    Ok(calendars.len())
}
