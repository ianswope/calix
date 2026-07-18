use chrono::Datelike;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use oauth2::reqwest;
use serde::{Deserialize, Serialize};
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
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Lists the calendars on the signed-in Google account.
pub fn list_calendars(access_token: &str) -> Result<Vec<CalendarListEntry>, String> {
    let url = Url::parse("https://www.googleapis.com/calendar/v3/users/me/calendarList")
        .map_err(|e| e.to_string())?;
    let mut calendars = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut page_url = url.clone();
        if let Some(page_token) = &page_token {
            page_url
                .query_pairs_mut()
                .append_pair("pageToken", page_token);
        }
        let body = get(access_token, page_url.as_str())?;
        let parsed: CalendarListResponse =
            serde_json::from_str(&body).map_err(|e| e.to_string())?;
        calendars.extend(parsed.items);
        page_token = parsed.next_page_token;
        if page_token.is_none() {
            break;
        }
    }

    Ok(calendars)
}

#[derive(Debug, Deserialize)]
pub struct EventItem {
    pub id: String,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "conferenceData")]
    pub conference_data: Option<ConferenceData>,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Debug, Deserialize)]
pub struct ConferenceData {
    #[serde(default, rename = "entryPoints")]
    entry_points: Vec<ConferenceEntryPoint>,
}

impl ConferenceData {
    pub fn join_links(&self) -> impl Iterator<Item = &str> {
        self.entry_points
            .iter()
            .filter(|entry| {
                matches!(
                    entry.entry_point_type.as_deref(),
                    Some("video") | Some("more")
                )
            })
            .map(|entry| entry.uri.as_str())
    }
}

#[derive(Debug, Deserialize)]
pub struct ConferenceEntryPoint {
    pub uri: String,
    #[serde(rename = "entryPointType")]
    entry_point_type: Option<String>,
}

impl EventItem {
    pub fn is_displayable_calendar_event(&self) -> bool {
        self.event_type.is_empty() || self.event_type == "default"
    }
}

#[derive(Debug, Deserialize)]
pub struct EventDateTime {
    /// Set (as `YYYY-MM-DD`) instead of `date_time` for all-day events.
    pub date: Option<String>,
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
    #[serde(rename = "timeZone")]
    pub time_zone: Option<String>,
}

impl EventDateTime {
    /// The moment this represents, plus whether it was an all-day
    /// (date-only) value. Returns `None` if Google sent neither field,
    /// which shouldn't happen for a well-formed event.
    pub fn to_local(&self) -> Option<(DateTime<Local>, bool)> {
        if let Some(date_time) = &self.date_time {
            if let Ok(parsed) = DateTime::parse_from_rfc3339(date_time) {
                return Some((parsed.with_timezone(&Local), false));
            }
            let timezone = self.time_zone.as_deref()?.parse::<chrono_tz::Tz>().ok()?;
            let naive = NaiveDateTime::parse_from_str(date_time, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
            let parsed = timezone.from_local_datetime(&naive).earliest()?;
            return Some((parsed.with_timezone(&Local), false));
        }
        let date = NaiveDate::parse_from_str(self.date.as_deref()?, "%Y-%m-%d").ok()?;
        Some((crate::date_util::local_day_start(date), true))
    }
}

#[derive(Deserialize)]
struct EventListResponse {
    #[serde(default)]
    items: Vec<EventItem>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
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
        .append_pair("orderBy", "startTime")
        .append_pair("conferenceDataVersion", "1");

    let mut events = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut page_url = url.clone();
        if let Some(page_token) = &page_token {
            page_url
                .query_pairs_mut()
                .append_pair("pageToken", page_token);
        }
        let body = get(access_token, page_url.as_str())?;
        let parsed: EventListResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        events.extend(
            parsed
                .items
                .into_iter()
                .filter(|event| event.status != "cancelled")
                .filter(EventItem::is_displayable_calendar_event),
        );
        page_token = parsed.next_page_token;
        if page_token.is_none() {
            break;
        }
    }
    Ok(events)
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

pub fn update_event(
    access_token: &str,
    calendar_id: &str,
    event_id: &str,
    draft: &crate::store::EventDraft,
) -> Result<(), String> {
    let url = event_url(calendar_id, event_id)?;
    let body = GoogleEventPatch::from_draft(draft);
    request_json(
        access_token,
        reqwest::Method::PATCH,
        url.as_str(),
        Some(&body),
    )
}

pub fn create_event(
    access_token: &str,
    calendar_id: &str,
    draft: &crate::store::EventDraft,
) -> Result<String, String> {
    let mut url = Url::parse("https://www.googleapis.com/calendar/v3/calendars")
        .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "invalid calendar API base URL".to_string())?
        .push(calendar_id)
        .push("events");
    let client = reqwest::blocking::Client::new();
    let body =
        serde_json::to_string(&GoogleEventPatch::from_draft(draft)).map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Google API error ({status}): {body}"));
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|e| e.to_string())?
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Google created an event without an ID".to_string())
}

pub fn delete_event(access_token: &str, calendar_id: &str, event_id: &str) -> Result<(), String> {
    let url = event_url(calendar_id, event_id)?;
    request_json::<()>(access_token, reqwest::Method::DELETE, url.as_str(), None)
}

fn event_url(calendar_id: &str, event_id: &str) -> Result<Url, String> {
    let mut url = Url::parse("https://www.googleapis.com/calendar/v3/calendars")
        .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "invalid calendar API base URL".to_string())?
        .push(calendar_id)
        .push("events")
        .push(event_id);
    Ok(url)
}

fn request_json<T: Serialize>(
    access_token: &str,
    method: reqwest::Method,
    url: &str,
    body: Option<&T>,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let mut request = client.request(method, url).bearer_auth(access_token);
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(body).map_err(|e| e.to_string())?);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Google API error ({status}): {body}"));
    }
    Ok(())
}

#[derive(Serialize)]
struct GoogleEventPatch {
    summary: String,
    location: Option<String>,
    description: Option<String>,
    start: GoogleEventDateTime,
    end: GoogleEventDateTime,
    // Omitted entirely for a one-off event so its payload is byte-for-byte what
    // Calix sent before recurrence existed (also leaves an expanded instance's
    // PATCH untouched). A recurring event sends `["RRULE:FREQ=…"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    recurrence: Option<Vec<String>>,
}

impl GoogleEventPatch {
    fn from_draft(draft: &crate::store::EventDraft) -> Self {
        Self {
            summary: draft.title.clone(),
            location: draft.location.clone(),
            description: draft.notes.clone(),
            start: GoogleEventDateTime::from_start(draft),
            end: GoogleEventDateTime::from_end(draft),
            recurrence: draft
                .recurrence
                .map(|freq| vec![format!("RRULE:{}", freq.to_rrule())]),
        }
    }
}

#[derive(Serialize)]
struct GoogleEventDateTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    #[serde(rename = "dateTime", skip_serializing_if = "Option::is_none")]
    date_time: Option<String>,
}

impl GoogleEventDateTime {
    fn from_start(draft: &crate::store::EventDraft) -> Self {
        if draft.all_day {
            Self {
                date: Some(date_string(draft.start)),
                date_time: None,
            }
        } else {
            Self {
                date: None,
                date_time: Some(draft.start.to_rfc3339()),
            }
        }
    }

    fn from_end(draft: &crate::store::EventDraft) -> Self {
        if draft.all_day {
            Self {
                date: Some(date_string(draft.end)),
                date_time: None,
            }
        } else {
            Self {
                date: None,
                date_time: Some(draft.end.to_rfc3339()),
            }
        }
    }
}

fn date_string(date: DateTime<Local>) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_event_type_filter_skips_working_location_events() {
        let event = EventItem {
            id: "evt-1".to_string(),
            summary: Some("Office".to_string()),
            location: None,
            description: None,
            status: "confirmed".to_string(),
            event_type: "workingLocation".to_string(),
            start: EventDateTime {
                date: Some("2026-07-09".to_string()),
                date_time: None,
                time_zone: None,
            },
            conference_data: None,
            end: EventDateTime {
                date: Some("2026-07-10".to_string()),
                date_time: None,
                time_zone: None,
            },
        };

        assert!(!event.is_displayable_calendar_event());
    }

    #[test]
    fn google_event_type_filter_keeps_default_events() {
        let event = EventItem {
            id: "evt-1".to_string(),
            summary: Some("Meeting".to_string()),
            location: None,
            description: None,
            status: "confirmed".to_string(),
            event_type: "default".to_string(),
            start: EventDateTime {
                date: None,
                date_time: Some("2026-07-09T10:00:00-05:00".to_string()),
                time_zone: None,
            },
            end: EventDateTime {
                date: None,
                date_time: Some("2026-07-09T11:00:00-05:00".to_string()),
                time_zone: None,
            },
            conference_data: None,
        };

        assert!(event.is_displayable_calendar_event());
    }

    #[test]
    fn google_patch_clears_absent_optional_fields() {
        let now = Local::now();
        let draft = crate::store::EventDraft {
            title: "Planning".to_string(),
            start: now,
            end: now + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
        };

        let json = serde_json::to_value(GoogleEventPatch::from_draft(&draft)).unwrap();

        assert!(json["location"].is_null());
        assert!(json["description"].is_null());
    }

    #[test]
    fn google_patch_serializes_recurrence_as_an_rrule_array_only_when_present() {
        let now = Local::now();
        let mut draft = crate::store::EventDraft {
            title: "Standup".to_string(),
            start: now,
            end: now + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            recurrence: Some(crate::recurrence::Frequency::Weekly),
        };

        let json = serde_json::to_value(GoogleEventPatch::from_draft(&draft)).unwrap();
        assert_eq!(json["recurrence"], serde_json::json!(["RRULE:FREQ=WEEKLY"]));

        // A one-off event omits the field so its payload is unchanged.
        draft.recurrence = None;
        let json = serde_json::to_value(GoogleEventPatch::from_draft(&draft)).unwrap();
        assert!(json.get("recurrence").is_none());
    }

    #[test]
    fn google_event_datetime_uses_its_declared_timezone_without_an_offset() {
        let datetime = EventDateTime {
            date: None,
            date_time: Some("2026-07-09T09:00:00".to_string()),
            time_zone: Some("America/New_York".to_string()),
        };

        let (local, all_day) = datetime.to_local().unwrap();

        assert!(!all_day);
        assert_eq!(
            local.with_timezone(&chrono::Utc),
            chrono::Utc.with_ymd_and_hms(2026, 7, 9, 13, 0, 0).unwrap()
        );
    }
}
