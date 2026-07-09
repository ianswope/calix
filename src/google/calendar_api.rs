use chrono::{DateTime, Local, NaiveDate, TimeZone};
use oauth2::reqwest;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize)]
pub struct CalendarListEntry {
    pub id: String,
    pub summary: String,
    #[serde(rename = "backgroundColor")]
    pub background_color: Option<String>,
    pub primary: Option<bool>,
    /// Whether the user has this calendar shown in their Google Calendar
    /// UI. Missing means shown (Google omits it rather than send `true`
    /// for a calendar's default state in some cases); only an explicit
    /// `false` means the user hid it.
    pub selected: Option<bool>,
}

impl CalendarListEntry {
    pub fn is_visible(&self) -> bool {
        self.selected != Some(false)
    }
}

#[derive(Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
}

/// Lists the calendars on the signed-in Google account.
pub fn list_calendars(access_token: &str) -> Result<Vec<CalendarListEntry>, String> {
    let url = "https://www.googleapis.com/calendar/v3/users/me/calendarList";
    let body = get(access_token, url)?;
    let parsed: CalendarListResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(parsed.items)
}

#[derive(Debug, Deserialize)]
pub struct EventItem {
    pub id: String,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub status: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Debug, Deserialize)]
pub struct EventDateTime {
    /// Set (as `YYYY-MM-DD`) instead of `date_time` for all-day events.
    pub date: Option<String>,
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
}

impl EventDateTime {
    /// The moment this represents, plus whether it was an all-day
    /// (date-only) value. Returns `None` if Google sent neither field,
    /// which shouldn't happen for a well-formed event.
    pub fn to_local(&self) -> Option<(DateTime<Local>, bool)> {
        if let Some(date_time) = &self.date_time {
            let parsed = DateTime::parse_from_rfc3339(date_time).ok()?;
            return Some((parsed.with_timezone(&Local), false));
        }
        let date = NaiveDate::parse_from_str(self.date.as_deref()?, "%Y-%m-%d").ok()?;
        let start_of_day = Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .single()?;
        Some((start_of_day, true))
    }
}

#[derive(Deserialize)]
struct EventListResponse {
    #[serde(default)]
    items: Vec<EventItem>,
}

/// Lists events on `calendar_id` overlapping [`time_min`, `time_max`),
/// with recurring events expanded into individual instances (each with
/// its own event id, so upserting keyed on id naturally handles them).
/// Cancelled instances are filtered out.
pub fn list_events(
    access_token: &str,
    calendar_id: &str,
    time_min: DateTime<Local>,
    time_max: DateTime<Local>,
) -> Result<Vec<EventItem>, String> {
    // No trailing slash: `path_segments_mut().push()` appends *after* the
    // path's current last segment, so a trailing "/" here (an empty final
    // segment) would leave a stray "//" before `calendar_id` — which Google
    // 404s on.
    let mut url = Url::parse("https://www.googleapis.com/calendar/v3/calendars")
        .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "invalid calendar API base URL".to_string())?
        .push(calendar_id)
        .push("events");
    url.query_pairs_mut()
        .append_pair("timeMin", &time_min.to_rfc3339())
        .append_pair("timeMax", &time_max.to_rfc3339())
        .append_pair("singleEvents", "true")
        .append_pair("orderBy", "startTime");

    let body = get(access_token, url.as_str())?;
    let parsed: EventListResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(parsed
        .items
        .into_iter()
        .filter(|e| e.status != "cancelled")
        .collect())
}

fn get(access_token: &str, url: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Google API error ({status}): {body}"));
    }
    Ok(body)
}
