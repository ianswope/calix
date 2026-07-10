use crate::store::EventDraft;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use oauth2::reqwest;
use std::collections::{HashMap, HashSet};

const ICLOUD_CALDAV_ROOT: &str = "https://caldav.icloud.com/";

pub struct Credentials {
    pub apple_id: String,
    pub app_password: String,
}

#[derive(Clone)]
pub struct RemoteCalendar {
    pub href: String,
    pub name: String,
    pub color: String,
}

pub struct RemoteEvent {
    pub href: String,
    pub draft: EventDraft,
}

pub fn discover_calendars(credentials: &Credentials) -> Result<Vec<RemoteCalendar>, String> {
    let principal = current_user_principal(credentials)?;
    let home = calendar_home_set(credentials, &absolute_url(&principal)?)?;
    let mut visited = HashSet::new();
    calendar_list(credentials, &absolute_url(&home)?, 0, &mut visited)
}

pub fn calendar_events(
    credentials: &Credentials,
    calendar_href: &str,
    start: DateTime<Local>,
    end: DateTime<Local>,
) -> Result<Vec<RemoteEvent>, String> {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data>
      <C:expand start="{}" end="{}"/>
    </C:calendar-data>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{}" end="{}"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#,
        caldav_timestamp(start),
        caldav_timestamp(end),
        caldav_timestamp(start),
        caldav_timestamp(end)
    );
    let response = request(
        credentials,
        "REPORT",
        &absolute_url(calendar_href)?,
        1,
        "application/xml; charset=utf-8",
        body,
    )?;

    Ok(multistatus_responses(&response)
        .into_iter()
        .flat_map(|response| {
            let href = child_text(&response, "href")?;
            let ics = child_text(&response, "calendar-data")?;
            Some(parse_events(&href, &ics))
        })
        .flatten()
        .collect())
}

pub fn update_event(
    credentials: &Credentials,
    event_href: &str,
    draft: &EventDraft,
) -> Result<(), String> {
    let (resource_href, recurrence_id) = event_href
        .split_once('#')
        .map_or((event_href, None), |(href, recurrence_id)| {
            (href, Some(recurrence_id))
        });
    let url = absolute_url(resource_href)?;
    let (existing_ics, etag) = fetch_event(credentials, &url)?;
    let ics = match recurrence_id {
        Some(recurrence_id) => replace_recurrence_instance(&existing_ics, recurrence_id, draft)?,
        None => replace_event_fields(&existing_ics, draft)?,
    };
    put_event(credentials, &url, &ics, etag.as_deref())?;
    Ok(())
}

pub fn create_event(
    credentials: &Credentials,
    calendar_href: &str,
    draft: &EventDraft,
) -> Result<String, String> {
    let uid = format!(
        "calix-{}-{}",
        chrono::Utc::now().timestamp_micros(),
        std::process::id()
    );
    let event_href = format!("{}/{}.ics", calendar_href.trim_end_matches('/'), uid);
    let ics = new_event_ics(&uid, draft);
    put_event(credentials, &absolute_url(&event_href)?, &ics, None)?;
    Ok(event_href)
}

fn fetch_event(credentials: &Credentials, url: &str) -> Result<(String, Option<String>), String> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(url)
        .basic_auth(&credentials.apple_id, Some(&credentials.app_password))
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("iCloud CalDAV error ({status}): {body}"));
    }
    Ok((body, etag))
}

fn put_event(
    credentials: &Credentials,
    url: &str,
    ics: &str,
    etag: Option<&str>,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let mut request = client
        .put(url)
        .basic_auth(&credentials.apple_id, Some(&credentials.app_password))
        .header("Content-Type", "text/calendar; charset=utf-8")
        .body(ics.to_owned());
    if let Some(etag) = etag {
        request = request.header("If-Match", etag);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("iCloud CalDAV error ({status}): {body}"));
    }
    Ok(())
}

pub fn delete_event(credentials: &Credentials, event_href: &str) -> Result<(), String> {
    if event_href.contains('#') {
        return Err(
            "Deleting expanded recurring iCloud instances is not supported yet".to_string(),
        );
    }
    let url = absolute_url(event_href)?;
    let (_, etag) = fetch_event(credentials, &url)?;
    let client = reqwest::blocking::Client::new();
    let mut request = client
        .delete(&url)
        .basic_auth(&credentials.apple_id, Some(&credentials.app_password));
    if let Some(etag) = etag.as_deref() {
        request = request.header("If-Match", etag);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("iCloud CalDAV error ({status}): {body}"));
    }
    Ok(())
}

fn current_user_principal(credentials: &Credentials) -> Result<String, String> {
    let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:current-user-principal/>
  </D:prop>
</D:propfind>"#;
    let response = request(
        credentials,
        "PROPFIND",
        ICLOUD_CALDAV_ROOT,
        0,
        "application/xml; charset=utf-8",
        body.to_string(),
    )
    .map_err(|error| format!("iCloud principal discovery failed: {error}"))?;
    child_xml(&response, "current-user-principal")
        .and_then(|principal| child_text(&principal, "href"))
        .ok_or_else(|| "iCloud did not return a principal URL".to_string())
}

fn calendar_home_set(credentials: &Credentials, principal_url: &str) -> Result<String, String> {
    let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-home-set/>
  </D:prop>
</D:propfind>"#;
    let response = request(
        credentials,
        "PROPFIND",
        principal_url,
        0,
        "application/xml; charset=utf-8",
        body.to_string(),
    )
    .map_err(|error| format!("iCloud calendar home discovery failed: {error}"))?;
    child_xml(&response, "calendar-home-set")
        .and_then(|home| child_text(&home, "href"))
        .ok_or_else(|| "iCloud did not return a calendar home URL".to_string())
}

fn calendar_list(
    credentials: &Credentials,
    collection_url: &str,
    depth: usize,
    visited: &mut HashSet<String>,
) -> Result<Vec<RemoteCalendar>, String> {
    if depth > 3 || !visited.insert(collection_url.to_string()) {
        return Ok(Vec::new());
    }

    let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:ICAL="http://apple.com/ns/ical/">
  <D:prop>
    <D:displayname/>
    <D:resourcetype/>
    <ICAL:calendar-color/>
    <C:supported-calendar-component-set/>
  </D:prop>
</D:propfind>"#;
    let response = request(
        credentials,
        "PROPFIND",
        collection_url,
        1,
        "application/xml; charset=utf-8",
        body.to_string(),
    )
    .map_err(|error| format!("iCloud calendar list failed: {error}"))?;

    let mut calendars = Vec::new();
    let mut child_collections = Vec::new();
    for response in multistatus_responses(&response) {
        let Some(href) = child_text(&response, "href") else {
            continue;
        };
        if should_skip_calendar_collection(&href) || same_collection(collection_url, &href) {
            continue;
        }

        if is_calendar_response(&response) {
            let name = child_text(&response, "displayname").unwrap_or_else(|| "iCloud".to_string());
            let color = child_text(&response, "calendar-color")
                .map(|color| color.chars().take(7).collect::<String>())
                .filter(|color| color.starts_with('#') && color.len() == 7)
                .unwrap_or_else(|| "#ff9500".to_string());
            calendars.push(RemoteCalendar { href, name, color });
        } else if is_collection_response(&response) {
            child_collections.push(href);
        }
    }

    for href in child_collections {
        let child_url = absolute_url(&href)?;
        calendars.extend(calendar_list(credentials, &child_url, depth + 1, visited)?);
    }

    Ok(calendars)
}

fn is_calendar_response(response: &str) -> bool {
    if response.contains("VEVENT") {
        return true;
    }

    child_xml(response, "resourcetype")
        .map(|resource_type| find_tag_start(&resource_type, "calendar").is_some())
        .unwrap_or(false)
}

fn is_collection_response(response: &str) -> bool {
    child_xml(response, "resourcetype")
        .map(|resource_type| find_tag_start(&resource_type, "collection").is_some())
        .unwrap_or(false)
}

fn should_skip_calendar_collection(href: &str) -> bool {
    let trimmed = href.trim_end_matches('/');
    trimmed.ends_with("/notification") || trimmed.ends_with("/outbox")
}

fn same_collection(collection_url: &str, href: &str) -> bool {
    collection_path(collection_url) == collection_path(href)
}

fn collection_path(url_or_href: &str) -> String {
    url::Url::parse(url_or_href)
        .map(|url| url.path().trim_end_matches('/').to_string())
        .unwrap_or_else(|_| url_or_href.trim_end_matches('/').to_string())
}

fn request(
    credentials: &Credentials,
    method: &str,
    url: &str,
    depth: u8,
    content_type: &str,
    body: String,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::new();
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let response = client
        .request(method, url)
        .basic_auth(&credentials.apple_id, Some(&credentials.app_password))
        .header("Depth", depth.to_string())
        .header("Content-Type", content_type)
        .body(body)
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() && status.as_u16() != 207 {
        return Err(format!("iCloud CalDAV error ({status}): {body}"));
    }
    Ok(body)
}

fn absolute_url(href: &str) -> Result<String, String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        Ok(href.to_string())
    } else {
        let root = url::Url::parse(ICLOUD_CALDAV_ROOT).map_err(|e| e.to_string())?;
        root.join(href)
            .map(|url| url.to_string())
            .map_err(|e| e.to_string())
    }
}

fn multistatus_responses(xml: &str) -> Vec<String> {
    let mut responses = Vec::new();
    let mut rest = xml;
    while let Some(start) = find_tag_start(rest, "response") {
        let after_start = &rest[start..];
        let Some(open_end) = after_start.find('>') else {
            break;
        };
        let content_start = start + open_end + 1;
        let Some(close_start) = find_closing_response(rest, content_start) else {
            break;
        };
        let close = &rest[close_start..];
        responses.push(rest[content_start..close_start].to_string());
        if let Some(close_end) = close.find('>') {
            rest = &close[close_end + 1..];
        } else {
            break;
        }
    }
    responses
}

fn find_closing_response(xml: &str, from: usize) -> Option<usize> {
    ["</D:response", "</d:response", "</response"]
        .into_iter()
        .filter_map(|tag| xml[from..].find(tag).map(|pos| from + pos))
        .min()
}

fn child_text(xml: &str, local_name: &str) -> Option<String> {
    let content = child_xml(xml, local_name)?;
    Some(xml_unescape(content.trim()))
}

fn child_xml(xml: &str, local_name: &str) -> Option<String> {
    let start = find_tag_start(xml, local_name)?;
    let after_start = &xml[start..];
    let open_end = after_start.find('>')?;
    if after_start[..open_end].ends_with('/') {
        return None;
    }
    let content_start = start + open_end + 1;
    let close_start = find_closing_tag(xml, local_name, content_start)?;
    Some(xml[content_start..close_start].to_string())
}

fn find_closing_tag(xml: &str, local_name: &str, from: usize) -> Option<usize> {
    let mut offset = from;
    while let Some(pos) = xml[offset..].find("</") {
        let start = offset + pos;
        let after = &xml[start + 2..];
        let name_end = after
            .find(|c: char| c == '>' || c.is_whitespace())
            .unwrap_or(after.len());
        let name = &after[..name_end];
        if name == local_name || name.rsplit(':').next() == Some(local_name) {
            return Some(start);
        }
        offset = start + 2;
    }
    None
}

fn find_tag_start(xml: &str, local_name: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(pos) = xml[offset..].find('<') {
        let start = offset + pos;
        let after = &xml[start + 1..];
        if after.starts_with('/') || after.starts_with('?') || after.starts_with('!') {
            offset = start + 1;
            continue;
        }
        let name_end = after
            .find(|c: char| c == '>' || c == '/' || c.is_whitespace())
            .unwrap_or(after.len());
        let name = &after[..name_end];
        if name == local_name || name.rsplit(':').next() == Some(local_name) {
            return Some(start);
        }
        offset = start + 1;
    }
    None
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn parse_events(href: &str, ics: &str) -> Vec<RemoteEvent> {
    let event_props = ics_event_properties(ics);
    let total = event_props.len();
    event_props
        .into_iter()
        .filter_map(|props| parse_event(href, props, total))
        .collect()
}

fn parse_event(
    href: &str,
    props: HashMap<String, IcsProperty>,
    component_count: usize,
) -> Option<RemoteEvent> {
    let summary = props
        .get("SUMMARY")
        .map(|property| property.value.clone())
        .unwrap_or_else(|| "(No title)".to_string());
    let (start, all_day) = parse_ics_datetime(props.get("DTSTART")?)?;
    let (end, _) = props
        .get("DTEND")
        .and_then(parse_ics_datetime)
        .unwrap_or_else(|| {
            if all_day {
                (start + chrono::Duration::days(1), true)
            } else {
                (start + chrono::Duration::hours(1), false)
            }
        });
    let remote_id = if component_count == 1 && !props.contains_key("RECURRENCE-ID") {
        href.to_string()
    } else {
        let instance_id = props
            .get("RECURRENCE-ID")
            .or_else(|| props.get("DTSTART"))
            .map(|property| property.value.clone())
            .unwrap_or_else(|| start.to_rfc3339());
        format!("{href}#{instance_id}")
    };

    Some(RemoteEvent {
        href: remote_id,
        draft: EventDraft {
            title: summary,
            start,
            end,
            all_day,
            location: props.get("LOCATION").map(|property| property.value.clone()),
            notes: props
                .get("DESCRIPTION")
                .map(|property| property.value.clone()),
        },
    })
}

struct IcsProperty {
    value: String,
    tzid: Option<String>,
}

fn ics_event_properties(ics: &str) -> Vec<HashMap<String, IcsProperty>> {
    let mut events = Vec::new();
    let mut props = HashMap::new();
    let mut in_event = false;
    for line in unfold_ics(ics) {
        if line == "BEGIN:VEVENT" {
            in_event = true;
            props = HashMap::new();
            continue;
        }
        if line == "END:VEVENT" {
            if in_event {
                events.push(std::mem::take(&mut props));
            }
            in_event = false;
            continue;
        }
        if !in_event {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = name.split(';');
        let key = parts.next().unwrap_or(name).to_string();
        let tzid = parts.find_map(|parameter| {
            parameter
                .split_once('=')
                .filter(|(key, _)| key.eq_ignore_ascii_case("TZID"))
                .map(|(_, value)| value.trim_matches('"').to_string())
        });
        props.insert(
            key,
            IcsProperty {
                value: unescape_ics_text(value),
                tzid,
            },
        );
    }
    events
}

fn parse_ics_datetime(property: &IcsProperty) -> Option<(DateTime<Local>, bool)> {
    let value = property.value.as_str();
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some((
            Local
                .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
                .single()?,
            true,
        ));
    }

    if let Some(stripped) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        let utc = chrono::Utc.from_utc_datetime(&naive);
        return Some((utc.with_timezone(&Local), false));
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    if let Some(tzid) = &property.tzid
        && let Ok(timezone) = tzid.parse::<Tz>()
    {
        let datetime = timezone.from_local_datetime(&naive).earliest()?;
        return Some((datetime.with_timezone(&Local), false));
    }
    Some((Local.from_local_datetime(&naive).single()?, false))
}

fn caldav_timestamp(dt: DateTime<Local>) -> String {
    dt.with_timezone(&chrono::Utc)
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn replace_event_fields(ics: &str, draft: &EventDraft) -> Result<String, String> {
    let event_count = unfold_ics(ics)
        .iter()
        .filter(|line| line.as_str() == "BEGIN:VEVENT")
        .count();
    if event_count != 1 {
        return Err("Editing recurring iCloud events is not supported yet".to_string());
    }

    let (start_key, start_value, end_key, end_value) = if draft.all_day {
        (
            "DTSTART;VALUE=DATE",
            draft.start.format("%Y%m%d").to_string(),
            "DTEND;VALUE=DATE",
            draft.end.format("%Y%m%d").to_string(),
        )
    } else {
        (
            "DTSTART",
            caldav_timestamp(draft.start),
            "DTEND",
            caldav_timestamp(draft.end),
        )
    };
    let mut replacement = vec![
        format!("DTSTAMP:{}", caldav_timestamp(Local::now())),
        format!("SUMMARY:{}", escape_ics_text(&draft.title)),
        format!("{start_key}:{start_value}"),
        format!("{end_key}:{end_value}"),
    ];
    if let Some(notes) = &draft.notes {
        replacement.push(format!("DESCRIPTION:{}", escape_ics_text(notes)));
    }
    if let Some(location) = &draft.location {
        replacement.push(format!("LOCATION:{}", escape_ics_text(location)));
    }

    let mut result = Vec::new();
    let mut in_event = false;
    for line in unfold_ics(ics) {
        if line == "BEGIN:VEVENT" {
            in_event = true;
            result.push(line);
            result.append(&mut replacement);
            continue;
        }
        if line == "END:VEVENT" {
            in_event = false;
            result.push(line);
            continue;
        }
        if in_event
            && property_name(&line).is_some_and(|name| {
                matches!(
                    name,
                    "DTSTAMP" | "SUMMARY" | "DTSTART" | "DTEND" | "LOCATION" | "DESCRIPTION"
                )
            })
        {
            continue;
        }
        result.push(line);
    }
    Ok(result.join("\r\n") + "\r\n")
}

fn replace_recurrence_instance(
    ics: &str,
    recurrence_id: &str,
    draft: &EventDraft,
) -> Result<String, String> {
    let lines = unfold_ics(ics);
    let uid = lines
        .iter()
        .find_map(|line| {
            (property_name(line) == Some("UID"))
                .then(|| line.split_once(':').map(|(_, value)| value.to_string()))
                .flatten()
        })
        .ok_or_else(|| "iCloud event is missing its UID".to_string())?;

    let mut result = Vec::new();
    let mut component = Vec::new();
    let mut in_event = false;
    for line in lines {
        if line == "BEGIN:VEVENT" {
            in_event = true;
            component.clear();
        }
        if in_event {
            component.push(line.clone());
        } else {
            result.push(line.clone());
        }
        if line == "END:VEVENT" {
            let is_replaced_instance = component.iter().any(|component_line| {
                property_name(component_line) == Some("RECURRENCE-ID")
                    && component_line
                        .split_once(':')
                        .is_some_and(|(_, value)| value == recurrence_id)
            });
            if !is_replaced_instance {
                result.append(&mut component);
            }
            in_event = false;
        }
    }

    let insert_at = result
        .iter()
        .position(|line| line == "END:VCALENDAR")
        .ok_or_else(|| "iCloud event is missing VCALENDAR closing data".to_string())?;
    result.splice(
        insert_at..insert_at,
        recurrence_exception_lines(&uid, recurrence_id, draft),
    );
    Ok(result.join("\r\n") + "\r\n")
}

fn recurrence_exception_lines(uid: &str, recurrence_id: &str, draft: &EventDraft) -> Vec<String> {
    let (start_key, start_value, end_key, end_value) = event_time_fields(draft);
    let recurrence_key = if recurrence_id.len() == 8
        && recurrence_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        "RECURRENCE-ID;VALUE=DATE"
    } else {
        "RECURRENCE-ID"
    };
    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("{recurrence_key}:{recurrence_id}"),
        format!("DTSTAMP:{}", caldav_timestamp(Local::now())),
        format!("SUMMARY:{}", escape_ics_text(&draft.title)),
        format!("{start_key}:{start_value}"),
        format!("{end_key}:{end_value}"),
    ];
    if let Some(location) = &draft.location {
        lines.push(format!("LOCATION:{}", escape_ics_text(location)));
    }
    if let Some(notes) = &draft.notes {
        lines.push(format!("DESCRIPTION:{}", escape_ics_text(notes)));
    }
    lines.push("END:VEVENT".to_string());
    lines
}

fn event_time_fields(draft: &EventDraft) -> (&'static str, String, &'static str, String) {
    if draft.all_day {
        (
            "DTSTART;VALUE=DATE",
            draft.start.format("%Y%m%d").to_string(),
            "DTEND;VALUE=DATE",
            draft.end.format("%Y%m%d").to_string(),
        )
    } else {
        (
            "DTSTART",
            caldav_timestamp(draft.start),
            "DTEND",
            caldav_timestamp(draft.end),
        )
    }
}

fn new_event_ics(uid: &str, draft: &EventDraft) -> String {
    let (start_key, start_value, end_key, end_value) = if draft.all_day {
        (
            "DTSTART;VALUE=DATE",
            draft.start.format("%Y%m%d").to_string(),
            "DTEND;VALUE=DATE",
            draft.end.format("%Y%m%d").to_string(),
        )
    } else {
        (
            "DTSTART",
            caldav_timestamp(draft.start),
            "DTEND",
            caldav_timestamp(draft.end),
        )
    };
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Calix//Calix Calendar//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}", escape_ics_text(uid)),
        format!("DTSTAMP:{}", caldav_timestamp(Local::now())),
        format!("SUMMARY:{}", escape_ics_text(&draft.title)),
        format!("{start_key}:{start_value}"),
        format!("{end_key}:{end_value}"),
    ];
    if let Some(location) = &draft.location {
        lines.push(format!("LOCATION:{}", escape_ics_text(location)));
    }
    if let Some(notes) = &draft.notes {
        lines.push(format!("DESCRIPTION:{}", escape_ics_text(notes)));
    }
    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n") + "\r\n"
}

fn unfold_ics(ics: &str) -> Vec<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in ics.replace("\r\n", "\n").lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = unfolded.last_mut() {
                last.push_str(line.trim_start());
            }
        } else {
            unfolded.push(line.to_string());
        }
    }
    unfolded
}

fn property_name(line: &str) -> Option<&str> {
    line.split_once(':')
        .map(|(name, _)| name.split(';').next().unwrap_or(name))
}

fn escape_ics_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

fn unescape_ics_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_xml_keeps_nested_children_until_matching_close_tag() {
        let xml = r#"
            <D:response>
              <D:href>/</D:href>
              <D:propstat>
                <D:prop>
                  <D:current-user-principal>
                    <D:href>/123456/principal/</D:href>
                  </D:current-user-principal>
                </D:prop>
              </D:propstat>
            </D:response>
        "#;

        let principal = child_xml(xml, "current-user-principal").unwrap();

        assert_eq!(
            child_text(&principal, "href").as_deref(),
            Some("/123456/principal/")
        );
    }

    #[test]
    fn child_text_still_reads_response_level_href() {
        let xml = r#"<D:response><D:href>/calendar/event.ics</D:href></D:response>"#;

        assert_eq!(
            child_text(xml, "href").as_deref(),
            Some("/calendar/event.ics")
        );
    }

    #[test]
    fn is_calendar_response_requires_calendar_resource_type() {
        let xml = r#"
            <D:response>
              <D:href>/99509935/calendars/</D:href>
              <D:propstat>
                <D:prop>
                  <D:resourcetype><D:collection/></D:resourcetype>
                </D:prop>
              </D:propstat>
            </D:response>
        "#;

        assert!(!is_calendar_response(xml));
    }

    #[test]
    fn is_calendar_response_accepts_caldav_calendar_resource_type() {
        let xml = r#"
            <D:response>
              <D:href>/99509935/calendars/personal/</D:href>
              <D:propstat>
                <D:prop>
                  <D:resourcetype>
                    <D:collection/>
                    <C:calendar xmlns:C="urn:ietf:params:xml:ns:caldav"/>
                  </D:resourcetype>
                </D:prop>
              </D:propstat>
            </D:response>
        "#;

        assert!(is_calendar_response(xml));
    }

    #[test]
    fn is_calendar_response_accepts_event_component_support() {
        let xml = r#"
            <D:response>
              <D:href>/99509935/calendars/shared/work/</D:href>
              <D:propstat>
                <D:prop>
                  <D:resourcetype><D:collection/></D:resourcetype>
                  <C:supported-calendar-component-set>
                    <C:comp name="VEVENT"/>
                  </C:supported-calendar-component-set>
                </D:prop>
              </D:propstat>
            </D:response>
        "#;

        assert!(is_calendar_response(xml));
    }

    #[test]
    fn should_skip_non_event_icloud_collections() {
        assert!(should_skip_calendar_collection(
            "/99509935/calendars/notification/"
        ));
        assert!(should_skip_calendar_collection(
            "/99509935/calendars/outbox/"
        ));
        assert!(!should_skip_calendar_collection(
            "/99509935/calendars/personal/"
        ));
    }

    #[test]
    fn same_collection_compares_paths_across_icloud_hosts() {
        assert!(same_collection(
            "https://p42-caldav.icloud.com/99509935/calendars/",
            "/99509935/calendars/"
        ));
    }

    #[test]
    fn parse_events_keeps_expanded_recurrence_instances_separate() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:Farren Fencing
DTSTART:20260709T183000Z
DTEND:20260709T213000Z
RECURRENCE-ID:20260709T183000Z
END:VEVENT
BEGIN:VEVENT
SUMMARY:Farren Fencing
DTSTART:20260716T183000Z
DTEND:20260716T213000Z
RECURRENCE-ID:20260716T183000Z
END:VEVENT
END:VCALENDAR"#;

        let events = parse_events("/99509935/calendars/farren/event.ics", ics);

        assert_eq!(events.len(), 2);
        assert_ne!(events[0].href, events[1].href);
        assert!(events[0].href.contains("20260709T183000Z"));
        assert!(events[1].href.contains("20260716T183000Z"));
    }

    #[test]
    fn parse_ics_datetime_uses_tzid() {
        let property = IcsProperty {
            value: "20260709T090000".to_string(),
            tzid: Some("America/New_York".to_string()),
        };

        let (datetime, all_day) = parse_ics_datetime(&property).unwrap();

        assert!(!all_day);
        assert_eq!(
            datetime.with_timezone(&chrono::Utc),
            chrono::Utc.with_ymd_and_hms(2026, 7, 9, 13, 0, 0).unwrap()
        );
    }

    #[test]
    fn replacing_event_fields_preserves_unedited_ics_properties() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc\r\nSUMMARY:Old title\r\nDTSTART;TZID=America/New_York:20260709T090000\r\nDTEND;TZID=America/New_York:20260709T100000\r\nRRULE:FREQ=WEEKLY\r\nATTENDEE:mailto:friend@example.com\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nLOCATION:Old location\r\nDESCRIPTION:Old notes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local::now();
        let draft = EventDraft {
            title: "New title".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
        };

        let updated = replace_event_fields(ics, &draft).unwrap();

        assert!(updated.contains("UID:abc"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert!(updated.contains("ATTENDEE:mailto:friend@example.com"));
        assert!(updated.contains("BEGIN:VALARM"));
        assert!(updated.contains("SUMMARY:New title"));
        assert!(!updated.contains("Old title"));
        assert!(!updated.contains("LOCATION:Old location"));
        assert!(!updated.contains("DESCRIPTION:Old notes"));
    }

    #[test]
    fn replacing_recurrence_instance_preserves_series_and_writes_exception() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART:20260709T140000Z\r\nDTEND:20260709T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local
            .with_ymd_and_hms(2026, 7, 10, 15, 0, 0)
            .single()
            .unwrap();
        let draft = EventDraft {
            title: "Moved standup".to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: None,
        };

        let updated = replace_recurrence_instance(ics, "20260709T140000Z", &draft).unwrap();

        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert!(updated.contains("UID:weekly-standup"));
        assert!(updated.contains("RECURRENCE-ID:20260709T140000Z"));
        assert!(updated.contains("SUMMARY:Moved standup"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 2);
    }
}
