use oauth2::reqwest;
use serde::Deserialize;

// id/summary aren't read yet — list_calendars is currently just a
// connectivity check; the sync pipeline that actually uses them is next.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct CalendarListEntry {
    pub id: String,
    pub summary: String,
}

#[derive(Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
}

/// Lists the calendars on the signed-in Google account. Used right now
/// just to verify a connection actually works; the sync pipeline that
/// pulls events in will follow once that's confirmed.
pub fn list_calendars(access_token: &str) -> Result<Vec<CalendarListEntry>, String> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get("https://www.googleapis.com/calendar/v3/users/me/calendarList")
        .bearer_auth(access_token)
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Google API error ({status}): {body}"));
    }

    let parsed: CalendarListResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(parsed.items)
}
