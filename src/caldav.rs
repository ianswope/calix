use crate::store::{Attendee, EventDraft, Store};
use crate::sync::SyncOutcome;
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use oauth2::reqwest;
use std::collections::{HashMap, HashSet};

/// How far back and forward each sync fetches events, in days.
const SYNC_PAST_DAYS: i64 = 90;
const SYNC_FUTURE_DAYS: i64 = 180;

/// Connection details for a CalDAV server. `base_url` is where discovery
/// starts (a server root, a `.well-known/caldav` URL, or a principal URL);
/// `username`/`password` are sent as HTTP Basic auth. iCloud is served by
/// pointing `base_url` at [`crate::icloud::ICLOUD_CALDAV_ROOT`] with an Apple
/// ID and app-specific password.
pub struct Credentials {
    pub base_url: String,
    pub username: String,
    pub password: String,
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
    pub attendees: Vec<Attendee>,
}

/// One `VEVENT`. Attendees are kept apart from `props` because `ATTENDEE`
/// repeats once per invitee, and a single-valued map would keep only the last.
struct IcsEvent {
    props: HashMap<String, IcsProperty>,
    attendees: Vec<Attendee>,
}

/// Parses one `ATTENDEE` line. The value is a CAL-ADDRESS, in practice always
/// `mailto:someone@example.com`; entries without a usable address are dropped
/// rather than listed blank.
fn parse_ics_attendee(parameters: &str, value: &str) -> Option<Attendee> {
    let email = mailto_address(value);
    if !email.contains('@') {
        return None;
    }

    let mut name = None;
    let mut status = None;
    // `parameters` still carries the property name, so skip that first segment.
    for parameter in parameters.split(';').skip(1) {
        let Some((key, raw_value)) = parameter.split_once('=') else {
            continue;
        };
        let raw_value = raw_value.trim_matches('"').trim();
        if raw_value.is_empty() {
            continue;
        }
        if key.eq_ignore_ascii_case("CN") {
            name = Some(raw_value.to_string());
        } else if key.eq_ignore_ascii_case("PARTSTAT") {
            status = normalize_partstat(raw_value);
        }
    }

    Some(Attendee {
        email: email.to_string(),
        name,
        status,
        is_self: false,
    })
}

/// Maps an iCalendar `PARTSTAT` onto the vocabulary the Google sync also uses,
/// so the UI renders one set of words. Unknown values are dropped.
fn normalize_partstat(partstat: &str) -> Option<String> {
    match partstat.to_ascii_uppercase().as_str() {
        "ACCEPTED" => Some("accepted".to_string()),
        "DECLINED" => Some("declined".to_string()),
        "TENTATIVE" => Some("tentative".to_string()),
        "NEEDS-ACTION" => Some("pending".to_string()),
        _ => None,
    }
}

pub fn discover_calendars(credentials: &Credentials) -> Result<Vec<RemoteCalendar>, String> {
    let principal = current_user_principal(credentials)?;
    let home = calendar_home_set(
        credentials,
        &absolute_url(&credentials.base_url, &principal)?,
    )?;
    let mut visited = HashSet::new();
    calendar_list(
        credentials,
        &absolute_url(&credentials.base_url, &home)?,
        0,
        &mut visited,
    )
}

/// Discovers a CalDAV account's calendars, syncs each one's events into the
/// store's CalDAV columns, and prunes rows that no longer exist server-side.
/// Used for both iCloud and generic `caldav` accounts — only the credentials
/// differ. Returns a [`SyncOutcome`] recording which calendars synced and which
/// failed.
pub fn sync_account(
    credentials: &Credentials,
    store: &Store,
    account_id: i64,
) -> Result<SyncOutcome, String> {
    let calendars = discover_calendars(credentials)?;
    let time_min = Local::now() - Duration::days(SYNC_PAST_DAYS);
    let time_max = Local::now() + Duration::days(SYNC_FUTURE_DAYS);
    let calendar_ids = calendars
        .iter()
        .map(|calendar| calendar.href.clone())
        .collect::<Vec<_>>();
    store
        .prune_caldav_calendars(account_id, &calendar_ids)
        .map_err(|e| e.to_string())?;

    let mut outcome = SyncOutcome::default();
    for calendar in &calendars {
        let local_calendar_id = store
            .upsert_caldav_calendar(
                account_id,
                &calendar.href,
                &calendar.name,
                &calendar.color,
                true,
            )
            .map_err(|e| e.to_string())?;

        let synced = match calendar_events(credentials, &calendar.href, time_min, time_max) {
            Ok(synced) => synced,
            Err(error) => {
                eprintln!(
                    "calix: failed to sync CalDAV calendar {} ({}): {}",
                    calendar.name, calendar.href, error
                );
                outcome.record_failure(calendar.name.clone());
                continue;
            }
        };
        for href in &synced.unreadable {
            eprintln!(
                "calix: keeping cached CalDAV events under {href} on {} — unreadable start/end",
                calendar.name
            );
        }
        let mut synced_ids = Vec::with_capacity(synced.events.len());
        for event in synced.events {
            store
                .upsert_caldav_event(
                    local_calendar_id,
                    &event.href,
                    &event.draft,
                    &event.attendees,
                )
                .map_err(|e| e.to_string())?;
            synced_ids.push(event.href);
        }
        if !synced.prunable {
            eprintln!(
                "calix: not pruning {} — the server sent a response with no href",
                calendar.name
            );
            outcome.record_failure(calendar.name.clone());
            continue;
        }
        store
            .prune_caldav_events(
                local_calendar_id,
                &synced_ids,
                &synced.unreadable,
                time_min,
                time_max,
            )
            .map_err(|e| e.to_string())?;
        outcome.record_success();
    }

    Ok(outcome)
}

/// One calendar-query's worth of events, split into what is safe to write and
/// what must not be deleted.
///
/// An event the parser can't read is still an event the server has. Deriving
/// the prune list from the parsed events alone deletes it from the cache — and
/// permanently, since the next sync won't parse it either — so the resources
/// the server accounted for are tracked apart from the events we can store.
pub struct CalendarSync {
    /// Events read in full — safe to upsert.
    pub events: Vec<RemoteEvent>,
    /// Resource hrefs the server returned but that we could not read in full.
    /// Every cached event under one of these survives pruning, whatever its
    /// instance id.
    pub unreadable: Vec<String>,
    /// False when some response could not be attributed to an href at all,
    /// which leaves us unable to say what the server still holds. The caller
    /// must then skip pruning rather than guess.
    pub prunable: bool,
}

pub fn calendar_events(
    credentials: &Credentials,
    calendar_href: &str,
    start: DateTime<Local>,
    end: DateTime<Local>,
) -> Result<CalendarSync, String> {
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
        &absolute_url(&credentials.base_url, calendar_href)?,
        1,
        "application/xml; charset=utf-8",
        body,
    )?;

    Ok(reconcile_calendar_query(&response))
}

/// Splits a multistatus calendar-query response into the events to upsert and
/// the resources that must survive pruning.
fn reconcile_calendar_query(xml: &str) -> CalendarSync {
    let mut events = Vec::new();
    let mut unreadable = Vec::new();
    let mut prunable = true;
    for response in multistatus_responses(xml) {
        let Some(href) = child_text(&response, "href") else {
            // Nothing ties this response to a resource, so the server's list of
            // what it still holds is incomplete in a way we can't localize.
            prunable = false;
            continue;
        };
        let Some(ics) = child_text(&response, "calendar-data") else {
            unreadable.push(href);
            continue;
        };
        let (parsed, complete) = parse_resource(&href, &ics);
        if !complete {
            unreadable.push(href);
        }
        events.extend(parsed);
    }
    CalendarSync {
        events,
        unreadable,
        prunable,
    }
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
    let url = absolute_url(&credentials.base_url, resource_href)?;
    let (existing_ics, etag) = fetch_event(credentials, &url)?;
    let ics = match recurrence_id {
        Some(recurrence_id) => replace_recurrence_instance(&existing_ics, recurrence_id, draft)?,
        None => replace_event_fields(&existing_ics, draft)?,
    };
    put_event(credentials, &url, &ics, etag.as_deref())?;
    Ok(())
}

pub fn respond_to_event(
    credentials: &Credentials,
    event_href: &str,
    attendee_email: &str,
    response: &str,
) -> Result<(), String> {
    let resource_href = event_href
        .split_once('#')
        .map_or(event_href, |(href, _)| href);
    let url = absolute_url(&credentials.base_url, resource_href)?;
    let (existing_ics, etag) = fetch_event(credentials, &url)?;
    let partstat = match response {
        "accepted" => "ACCEPTED",
        "declined" => "DECLINED",
        "tentative" => "TENTATIVE",
        _ => return Err("Unknown invitation response".to_string()),
    };
    let ics = reply_to_invitation(&existing_ics, attendee_email, partstat)?;
    put_event(credentials, &url, &ics, etag.as_deref())
}

/// The resource with `attendee_email`'s reply recorded: every `ATTENDEE` line
/// naming that address gets `PARTSTAT={partstat}` in place of whatever reply
/// it carried, and every other line is left exactly as it was.
fn reply_to_invitation(ics: &str, attendee_email: &str, partstat: &str) -> Result<String, String> {
    let mut found = false;
    let lines = unfold_ics(ics)
        .into_iter()
        .map(|line| {
            let is_attendee =
                property_name(&line).is_some_and(|name| name.eq_ignore_ascii_case("ATTENDEE"));
            let Some((head, value)) = split_content_line(&line).filter(|_| is_attendee) else {
                return line;
            };
            if !mailto_address(value).eq_ignore_ascii_case(attendee_email) {
                return line;
            }
            found = true;
            // The value — `mailto:` and all — is carried over untouched; only
            // the PARTSTAT parameter is replaced.
            let mut parameters = split_parameters(head)
                .into_iter()
                .filter(|parameter| !parameter.to_ascii_uppercase().starts_with("PARTSTAT="))
                .map(str::to_string)
                .collect::<Vec<_>>();
            parameters.push(format!("PARTSTAT={partstat}"));
            format!("{}:{value}", parameters.join(";"))
        })
        .collect::<Vec<_>>();
    if !found {
        return Err("The server did not identify your invitation on this event".to_string());
    }
    Ok(lines.join("\r\n") + "\r\n")
}

/// How far an "all events" edit moves a series, in the two forms an iCalendar
/// date-time takes. A UTC value (`…Z`) names an instant, so it moves by the
/// exact elapsed time. A value in a named zone, a floating one, or a bare date
/// names a wall-clock reading, so it moves by the change in that reading —
/// which differs from the exact one by an hour whenever the move crosses a DST
/// transition, and is what keeps a 9 AM series at 9 AM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesShift {
    exact: chrono::Duration,
    wall_clock: chrono::Duration,
}

impl SeriesShift {
    /// The shift that takes an occurrence starting at `from` to one starting
    /// at `to`.
    pub fn between<Tz: TimeZone>(from: DateTime<Tz>, to: DateTime<Tz>) -> Self {
        Self {
            wall_clock: to.naive_local() - from.naive_local(),
            exact: to - from,
        }
    }

    fn is_zero(self) -> bool {
        self.exact.is_zero() && self.wall_clock.is_zero()
    }
}

/// Edits a whole series ("all events") from one of its occurrences, moving the
/// series by `shift` (how far that occurrence's start moved). `event_href` may
/// be either the master resource or an `href#instance`.
pub fn update_series(
    credentials: &Credentials,
    event_href: &str,
    shift: SeriesShift,
    draft: &EventDraft,
) -> Result<(), String> {
    let resource_href = event_href
        .split_once('#')
        .map_or(event_href, |(href, _)| href);
    let url = absolute_url(&credentials.base_url, resource_href)?;
    let (existing_ics, etag) = fetch_event(credentials, &url)?;
    let ics = edit_master_series(&existing_ics, shift, draft)?;
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
    put_event(
        credentials,
        &absolute_url(&credentials.base_url, &event_href)?,
        &ics,
        None,
    )?;
    Ok(event_href)
}

fn fetch_event(credentials: &Credentials, url: &str) -> Result<(String, Option<String>), String> {
    let client = crate::http::client()?;
    let response = client
        .get(url)
        .basic_auth(&credentials.username, Some(&credentials.password))
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
        return Err(http_error(status.as_u16(), &body, is_icloud(credentials)));
    }
    Ok((body, etag))
}

fn put_event(
    credentials: &Credentials,
    url: &str,
    ics: &str,
    etag: Option<&str>,
) -> Result<(), String> {
    let client = crate::http::client()?;
    let mut request = client
        .put(url)
        .basic_auth(&credentials.username, Some(&credentials.password))
        .header("Content-Type", "text/calendar; charset=utf-8")
        .body(ics.to_owned());
    if let Some(etag) = etag {
        request = request.header("If-Match", etag);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_error(status.as_u16(), &body, is_icloud(credentials)));
    }
    Ok(())
}

pub fn delete_event(credentials: &Credentials, event_href: &str) -> Result<(), String> {
    // Deleting one occurrence of a series excludes it from the master rather
    // than removing the whole resource, mirroring the single-instance edit path.
    if let Some((resource_href, recurrence_id)) = event_href.split_once('#') {
        let url = absolute_url(&credentials.base_url, resource_href)?;
        let (existing_ics, etag) = fetch_event(credentials, &url)?;
        let ics = exclude_recurrence_instance(&existing_ics, recurrence_id)?;
        put_event(credentials, &url, &ics, etag.as_deref())?;
        return Ok(());
    }
    let url = absolute_url(&credentials.base_url, event_href)?;
    let (_, etag) = fetch_event(credentials, &url)?;
    let client = crate::http::client()?;
    let mut request = client
        .delete(&url)
        .basic_auth(&credentials.username, Some(&credentials.password));
    if let Some(etag) = etag.as_deref() {
        request = request.header("If-Match", etag);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_error(status.as_u16(), &body, is_icloud(credentials)));
    }
    Ok(())
}

fn current_user_principal(credentials: &Credentials) -> Result<String, String> {
    // Try the URL the user gave first; if the server doesn't answer
    // current-user-principal there (common when they paste a bare origin
    // like https://caldav.fastmail.com), fall back to the RFC 6764
    // /.well-known/caldav bootstrap. iCloud answers directly at its root, so
    // the fallback never fires for it.
    if let Some(principal) = principal_at(credentials, &credentials.base_url)? {
        return Ok(principal);
    }
    let well_known = absolute_url(&credentials.base_url, "/.well-known/caldav")?;
    if let Some(principal) = principal_at(credentials, &well_known)? {
        return Ok(principal);
    }
    Err("The server did not return a CalDAV principal URL. Check the server address.".to_string())
}

fn principal_at(credentials: &Credentials, url: &str) -> Result<Option<String>, String> {
    let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:current-user-principal/>
  </D:prop>
</D:propfind>"#;
    let response = request(
        credentials,
        "PROPFIND",
        url,
        0,
        "application/xml; charset=utf-8",
        body.to_string(),
    )
    .map_err(|error| format!("CalDAV principal discovery failed: {error}"))?;
    Ok(child_xml(&response, "current-user-principal")
        .and_then(|principal| child_text(&principal, "href")))
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
    .map_err(|error| format!("CalDAV calendar home discovery failed: {error}"))?;
    child_xml(&response, "calendar-home-set")
        .and_then(|home| child_text(&home, "href"))
        .ok_or_else(|| "The server did not return a calendar home URL".to_string())
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
    .map_err(|error| format!("CalDAV calendar list failed: {error}"))?;

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
            let name =
                child_text(&response, "displayname").unwrap_or_else(|| "Calendar".to_string());
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
        let child_url = absolute_url(&credentials.base_url, &href)?;
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

/// Parses and canonicalizes a CalDAV base URL — lowercased scheme and host,
/// default port dropped, no trailing slash — so equivalent spellings like
/// `https://Host/` and `https://host` map to one account row and one keyring
/// entry instead of duplicate accounts sharing a secret.
pub fn canonical_base_url(input: &str) -> Result<String, String> {
    let url = url::Url::parse(input.trim()).map_err(|e| format!("Invalid server URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("The server URL must start with http:// or https://.".to_string());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
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
    let client = crate::http::client()?;
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let response = client
        .request(method, url)
        .basic_auth(&credentials.username, Some(&credentials.password))
        .header("Depth", depth.to_string())
        .header("Content-Type", content_type)
        .body(body)
        .send()
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() && status.as_u16() != 207 {
        return Err(http_error(status.as_u16(), &body, is_icloud(credentials)));
    }
    Ok(body)
}

/// Renders a failed CalDAV response as a message that says what to do next.
///
/// A 401 is the one status that actually means "this credential is dead": for
/// iCloud that happens when the app-specific password is revoked, which
/// changing your Apple ID password does to all of them at once. Every other
/// status is a server- or request-level problem that generating a new password
/// would not fix, so the message must not imply otherwise — an unexplained 5xx
/// that reads like an auth failure is what sends you to Apple's website to
/// mint a password you did not need.
///
/// `icloud` picks which recovery advice a 401 carries. This module drives
/// Fastmail, Nextcloud and self-hosted servers too, and sending one of those
/// users to `account.apple.com` is a dead end that reads like the app is
/// confused about which account it just failed to sync.
fn http_error(status: u16, body: &str, icloud: bool) -> String {
    if status == 401 {
        return if icloud {
            "CalDAV rejected the saved credential (401 Unauthorized). \
             Generate a new app-specific password at account.apple.com and \
             reconnect the account — changing your Apple ID password revokes \
             every app-specific password at once."
                .to_string()
        } else {
            "CalDAV rejected the saved credential (401 Unauthorized). \
             Check this account's username and password on the server and \
             reconnect it — some servers want an app password generated for \
             Calix rather than your login password."
                .to_string()
        };
    }
    format!("CalDAV error ({status}): {body}")
}

/// Resolves a possibly-relative href (as CalDAV servers return in multistatus
/// responses) against the server's base URL. Every request attaches the
/// account's Basic-auth credentials, so absolute hrefs are only accepted on
/// the configured origin — plus iCloud's partition hosts (iCloud routes
/// principals to hosts like `p42-caldav.icloud.com`) over HTTPS. Anything
/// else would let a hostile server redirect the credentials elsewhere or
/// downgrade them to cleartext HTTP.
fn absolute_url(base_url: &str, href: &str) -> Result<String, String> {
    let root = url::Url::parse(base_url).map_err(|e| e.to_string())?;
    let resolved = root.join(href).map_err(|e| e.to_string())?;
    if resolved.origin() == root.origin() || is_icloud_partition_pair(&root, &resolved) {
        Ok(resolved.to_string())
    } else {
        Err(format!(
            "CalDAV server returned an href on an unexpected host: {resolved}"
        ))
    }
}

fn is_icloud_partition_pair(root: &url::Url, resolved: &url::Url) -> bool {
    https_icloud_host(root) && https_icloud_host(resolved)
}

fn https_icloud_host(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "icloud.com" || host.ends_with(".icloud.com"))
}

/// Whether an account is iCloud, which is the one thing this module needs a
/// provider distinction for: only Apple has app-specific passwords to
/// regenerate. Everything else here is the same for Fastmail, Nextcloud or a
/// self-hosted server, which is why the account's own URL is enough to tell —
/// there is no provider tag to thread down here.
fn is_icloud(credentials: &Credentials) -> bool {
    url::Url::parse(&credentials.base_url).is_ok_and(|url| https_icloud_host(&url))
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
        let Some(close_start) = find_closing_tag(rest, "response", content_start) else {
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

/// Decodes XML character references in one pass, so each `&…;` is read exactly
/// once: a chain of `replace` calls turned `&amp;lt;` into `<` by decoding the
/// `&amp;` and then decoding what it produced. Anything that isn't a reference
/// this recognizes is left as written.
fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let reference = &rest[start..];
        let decoded = reference
            .find(';')
            .and_then(|end| decode_xml_entity(&reference[1..end]).map(|c| (c, end)));
        match decoded {
            Some((character, end)) => {
                out.push(character);
                rest = &reference[end + 1..];
            }
            None => {
                out.push('&');
                rest = &reference[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character an entity name stands for: the five XML predefines it, plus
/// decimal and hex character references (`#39`, `#x27`).
fn decode_xml_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Parses every `VEVENT` in one resource, reporting whether any was dropped.
///
/// A dropped instance has to protect its whole resource rather than just
/// itself: the id a cached instance is stored under is derived from the very
/// `DTSTART`/`RECURRENCE-ID` that failed to parse, so there is no way to name
/// the instance that went missing.
fn parse_resource(href: &str, ics: &str) -> (Vec<RemoteEvent>, bool) {
    let components = ics_event_properties(ics);
    let total = components.len();
    let events = components
        .into_iter()
        .filter_map(|component| parse_event(href, component, total))
        .collect::<Vec<_>>();
    let complete = events.len() == total;
    (events, complete)
}

fn parse_event(href: &str, component: IcsEvent, component_count: usize) -> Option<RemoteEvent> {
    let IcsEvent { props, attendees } = component;
    let summary = props
        .get("SUMMARY")
        .map(|property| property.value.clone())
        .unwrap_or_else(|| "(No title)".to_string());
    let (start, all_day) = parse_ics_datetime(props.get("DTSTART")?)?;
    let end = event_end(&props, start, all_day)?;
    let remote_id = if component_count == 1 && !props.contains_key("RECURRENCE-ID") {
        href.to_string()
    } else {
        let instance_id = props
            .get("RECURRENCE-ID")
            .or_else(|| props.get("DTSTART"))
            .map(recurrence_instance_id)
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
            // Events are fetched with server-side <C:expand>, so each occurrence
            // arrives as its own one-off VEVENT without an RRULE.
            recurrence: None,
            reminder_minutes: None,
            attendees: attendees.clone(),
        },
        attendees,
    })
}

struct IcsProperty {
    value: String,
    tzid: Option<String>,
}

fn ics_event_properties(ics: &str) -> Vec<IcsEvent> {
    let mut events = Vec::new();
    let mut props = HashMap::new();
    let mut attendees: Vec<Attendee> = Vec::new();
    let mut in_event = false;
    // Depth of nested components (e.g. VALARM) below the VEVENT. Only the
    // VEVENT's own properties — depth 0 — are the event's; an alarm's
    // `DESCRIPTION`/`DTSTART` must not overwrite the event's own.
    let mut nested_depth = 0usize;
    for line in unfold_ics(ics) {
        if is_component_boundary(&line, "BEGIN", "VEVENT") {
            in_event = true;
            nested_depth = 0;
            props = HashMap::new();
            attendees = Vec::new();
            continue;
        }
        if is_component_boundary(&line, "END", "VEVENT") {
            if in_event {
                events.push(IcsEvent {
                    props: std::mem::take(&mut props),
                    attendees: std::mem::take(&mut attendees),
                });
            }
            in_event = false;
            continue;
        }
        if !in_event {
            continue;
        }
        if is_component_keyword(&line, "BEGIN") {
            nested_depth += 1;
            continue;
        }
        if is_component_keyword(&line, "END") {
            nested_depth = nested_depth.saturating_sub(1);
            continue;
        }
        if nested_depth > 0 {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = name.split(';');
        let key = parts.next().unwrap_or(name).to_ascii_uppercase();
        if key == "ATTENDEE" {
            if let Some(attendee) = parse_ics_attendee(name, value) {
                // Servers sometimes repeat an invitee across CUTYPE/ROLE
                // variants; keep the first mention of each address.
                if !attendees
                    .iter()
                    .any(|existing| existing.email.eq_ignore_ascii_case(&attendee.email))
                {
                    attendees.push(attendee);
                }
            }
            continue;
        }
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

/// True if `line` is the boundary `BEGIN:`/`END:` of the given component,
/// matched case-insensitively (iCalendar names are case-insensitive).
fn is_component_boundary(line: &str, keyword: &str, component: &str) -> bool {
    line.split_once(':').is_some_and(|(name, value)| {
        name.eq_ignore_ascii_case(keyword) && value.eq_ignore_ascii_case(component)
    })
}

/// True if `line` opens (`BEGIN:`) or closes (`END:`) any component.
fn is_component_keyword(line: &str, keyword: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(name, _)| name.eq_ignore_ascii_case(keyword))
}

fn parse_ics_datetime(property: &IcsProperty) -> Option<(DateTime<Local>, bool)> {
    parse_ics_datetime_in(property, &Local)
}

/// [`parse_ics_datetime`] with the zone a floating value (no `Z`, no `TZID`)
/// is read in made explicit, so the resolution can be tested against a fixed
/// zone rather than whatever the machine running the tests is set to.
fn parse_ics_datetime_in<Zone: TimeZone>(
    property: &IcsProperty,
    floating_zone: &Zone,
) -> Option<(DateTime<Local>, bool)> {
    let value = property.value.as_str();
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some((crate::date_util::local_day_start(date), true));
    }

    if let Some(stripped) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        let utc = chrono::Utc.from_utc_datetime(&naive);
        return Some((utc.with_timezone(&Local), false));
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    // A named zone, or the floating fallback when there is none (or one this
    // build doesn't know): either way the value is a wall-clock reading, and a
    // reading the clocks skipped or repeated is resolved forward rather than
    // declared unreadable — see `date_util::resolve_forward`.
    let named_zone = property
        .tzid
        .as_deref()
        .and_then(|tzid| tzid.parse::<Tz>().ok());
    let datetime = match named_zone {
        Some(zone) => crate::date_util::resolve_forward(&zone, naive).with_timezone(&Local),
        None => crate::date_util::resolve_forward(floating_zone, naive).with_timezone(&Local),
    };
    Some((datetime, false))
}

/// An RFC 5545 `DURATION`, keeping nominal days apart from the exact time
/// span. The spec makes day and week durations nominal: "one day later" means
/// the same clock time on the next civil date, which is 23 or 25 elapsed hours
/// across a DST transition, not 24.
struct IcsDuration {
    days: i64,
    time: chrono::Duration,
}

/// Parses a `DURATION` value such as `P1D`, `PT1H30M`, `P2W`, or `P1DT12H`.
fn parse_ics_duration(value: &str) -> Option<IcsDuration> {
    let (sign, rest) = match value.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (rest, None),
    };
    if date_part.is_empty() && time_part.is_none_or(str::is_empty) {
        return None;
    }

    let mut days = 0i64;
    for (count, unit) in measures(date_part)? {
        match unit {
            'W' => days += count * 7,
            'D' => days += count,
            _ => return None,
        }
    }
    let mut seconds = 0i64;
    for (count, unit) in measures(time_part.unwrap_or_default())? {
        match unit {
            'H' => seconds += count * 3600,
            'M' => seconds += count * 60,
            'S' => seconds += count,
            _ => return None,
        }
    }

    Some(IcsDuration {
        days: sign * days,
        time: chrono::Duration::seconds(sign * seconds),
    })
}

/// Splits a duration part into its `(count, unit)` measures, rejecting a
/// trailing count with no unit — `P1D5` must not read as one day.
fn measures(part: &str) -> Option<Vec<(i64, char)>> {
    let mut measures = Vec::new();
    let mut digits = String::new();
    for character in part.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        measures.push((digits.parse().ok()?, character));
        digits.clear();
    }
    digits.is_empty().then_some(measures)
}

/// The instant `duration` after `start`.
///
/// An all-day event advances by whole civil days, so its end lands on the next
/// date's first instant however the offset moved in between.
fn end_from_duration<Tz: TimeZone>(
    start: &DateTime<Tz>,
    all_day: bool,
    duration: &IcsDuration,
) -> DateTime<Tz> {
    let days = chrono::Duration::days(duration.days);
    let shifted = if all_day {
        // Land on the next date's first instant, whatever the offset did in
        // between. RFC 5545 only allows day and week durations on a DATE start,
        // so there is no time-of-day to carry across.
        crate::date_util::day_start_in(&start.timezone(), start.date_naive() + days)
    } else {
        start.clone() + days
    };
    shifted + duration.time
}

/// The event's end: `DTEND` if it has one, otherwise `DURATION`, otherwise a
/// fallback.
///
/// A `DTEND` or `DURATION` that is present but unreadable yields `None` rather
/// than a fallback. Inventing an end for a property the server did send — and
/// that a later edit would write back — is worse than declining to read the
/// event, which leaves the cached copy in place.
fn event_end(
    props: &HashMap<String, IcsProperty>,
    start: DateTime<Local>,
    all_day: bool,
) -> Option<DateTime<Local>> {
    if let Some(dtend) = props.get("DTEND") {
        return parse_ics_datetime(dtend).map(|(end, _)| end);
    }
    if let Some(duration) = props.get("DURATION") {
        let duration = parse_ics_duration(&duration.value)?;
        return Some(end_from_duration(&start, all_day, &duration));
    }
    // Neither property: RFC 5545 gives a DATE start a one-day duration, and a
    // DATE-TIME start a zero-length one. A zero-length event would be invisible
    // in the grids, so a timed event without an end gets a nominal hour — the
    // one invented end that is a display decision rather than a parse failure.
    Some(if all_day {
        end_from_duration(
            &start,
            true,
            &IcsDuration {
                days: 1,
                time: chrono::Duration::zero(),
            },
        )
    } else {
        start + chrono::Duration::hours(1)
    })
}

fn caldav_timestamp(dt: DateTime<Local>) -> String {
    dt.with_timezone(&chrono::Utc)
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn replace_event_fields(ics: &str, draft: &EventDraft) -> Result<String, String> {
    // One VEVENT with no rule of its own is the only thing this rewrites. A
    // resource holding several is a series with overrides, and one whose single
    // VEVENT carries an RRULE or RDATE is a series the server handed back
    // unexpanded — its DTSTART anchors every occurrence, so rewriting it to the
    // one that was clicked would move them all and drop the zone they repeat in.
    let components = ics_event_properties(ics);
    let is_series = components.len() != 1
        || components[0].props.contains_key("RRULE")
        || components[0].props.contains_key("RDATE");
    if is_series {
        return Err(
            "Editing this repeating event isn't supported yet — the server sent the whole \
             series as one item"
                .to_string(),
        );
    }

    let (start_key, start_value, end_key, end_value) = event_time_fields(draft);
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
    // Depth of nested components (e.g. VALARM): the fields we rewrite must only
    // be stripped from the VEVENT itself, never from an alarm that carries its
    // own `DESCRIPTION`/`DTSTART`.
    let mut nested_depth = 0usize;
    for line in unfold_ics(ics) {
        if is_component_boundary(&line, "BEGIN", "VEVENT") {
            in_event = true;
            nested_depth = 0;
            result.push(line);
            result.append(&mut replacement);
            continue;
        }
        if is_component_boundary(&line, "END", "VEVENT") {
            in_event = false;
            result.push(line);
            continue;
        }
        if in_event && is_component_keyword(&line, "BEGIN") {
            nested_depth += 1;
            result.push(line);
            continue;
        }
        if in_event && is_component_keyword(&line, "END") {
            nested_depth = nested_depth.saturating_sub(1);
            result.push(line);
            continue;
        }
        if in_event
            && nested_depth == 0
            && property_name(&line).is_some_and(|name| {
                matches!(
                    name.to_ascii_uppercase().as_str(),
                    // DURATION goes too: the draft's end is written as a
                    // DTEND, and RFC 5545 forbids carrying both.
                    "DTSTAMP"
                        | "SUMMARY"
                        | "DTSTART"
                        | "DTEND"
                        | "DURATION"
                        | "LOCATION"
                        | "DESCRIPTION"
                )
            })
        {
            continue;
        }
        result.push(line);
    }
    Ok(result.join("\r\n") + "\r\n")
}

/// Cached identity of an expanded recurrence instance: the `RECURRENCE-ID`
/// value qualified by its `TZID` parameter (`TZID=Zone:value`), so the
/// write-back path can reproduce the exact property form the series used
/// instead of emitting a floating timestamp that names a different — or no —
/// occurrence. Bare UTC (`...Z`) and all-day (`YYYYMMDD`) values carry no
/// parameter and stay bare.
fn recurrence_instance_id(property: &IcsProperty) -> String {
    match &property.tzid {
        Some(tzid) => format!("TZID={tzid}:{}", property.value),
        None => property.value.clone(),
    }
}

/// Splits a `recurrence_instance_id` identity back into its optional TZID
/// and raw datetime value.
fn split_recurrence_id(recurrence_id: &str) -> (Option<&str>, &str) {
    recurrence_id
        .strip_prefix("TZID=")
        .and_then(|rest| rest.split_once(':'))
        .map(|(tzid, value)| (Some(tzid), value))
        .unwrap_or((None, recurrence_id))
}

fn replace_recurrence_instance(
    ics: &str,
    recurrence_id: &str,
    draft: &EventDraft,
) -> Result<String, String> {
    let (_, recurrence_value) = split_recurrence_id(recurrence_id);
    let lines = unfold_ics(ics);
    let uid = lines
        .iter()
        .find_map(|line| {
            property_name(line)
                .is_some_and(|name| name.eq_ignore_ascii_case("UID"))
                .then(|| line.split_once(':').map(|(_, value)| value.to_string()))
                .flatten()
        })
        .ok_or_else(|| "Event is missing its UID".to_string())?;

    let mut result = Vec::new();
    let mut component = Vec::new();
    let mut in_event = false;
    for line in lines {
        if is_component_boundary(&line, "BEGIN", "VEVENT") {
            in_event = true;
            component.clear();
        }
        if in_event {
            component.push(line.clone());
        } else {
            result.push(line.clone());
        }
        if is_component_boundary(&line, "END", "VEVENT") {
            let is_replaced_instance = component.iter().any(|component_line| {
                property_name(component_line)
                    .is_some_and(|name| name.eq_ignore_ascii_case("RECURRENCE-ID"))
                    && component_line
                        .split_once(':')
                        .is_some_and(|(_, value)| value == recurrence_value)
            });
            if !is_replaced_instance {
                result.append(&mut component);
            }
            in_event = false;
        }
    }

    let insert_at = result
        .iter()
        .position(|line| is_component_boundary(line, "END", "VCALENDAR"))
        .ok_or_else(|| "Event is missing VCALENDAR closing data".to_string())?;
    result.splice(
        insert_at..insert_at,
        recurrence_exception_lines(&uid, recurrence_id, draft),
    );
    Ok(result.join("\r\n") + "\r\n")
}

/// Edits every occurrence of a series (an "all events" edit made from one
/// occurrence): shifts the master `VEVENT`'s start/end by `shift` (so the
/// whole series moves by however far that occurrence moved, keeping its
/// timezone form and recurrence pattern) and replaces its summary/location/
/// notes from `draft`. Override `VEVENT`s and everything else are left as-is.
fn edit_master_series(ics: &str, shift: SeriesShift, draft: &EventDraft) -> Result<String, String> {
    let mut result = Vec::new();
    let mut component: Vec<String> = Vec::new();
    let mut in_event = false;
    let mut edited = false;
    for line in unfold_ics(ics) {
        if is_component_boundary(&line, "BEGIN", "VEVENT") {
            in_event = true;
            component.clear();
        }
        if in_event {
            component.push(line.clone());
        } else {
            result.push(line.clone());
        }
        if is_component_boundary(&line, "END", "VEVENT") {
            in_event = false;
            // The master is the VEVENT without a RECURRENCE-ID; override
            // VEVENTs (customized single instances) are copied through as-is.
            let is_master = !component.iter().any(|component_line| {
                property_name(component_line)
                    .is_some_and(|name| name.eq_ignore_ascii_case("RECURRENCE-ID"))
            });
            if is_master {
                edited = true;
                result.extend(rewrite_master_vevent(&component, shift, draft));
            } else {
                result.append(&mut component);
            }
            component.clear();
        }
    }
    if !edited {
        return Err("Could not find the series to edit".to_string());
    }
    Ok(result.join("\r\n") + "\r\n")
}

/// Rewrites the master `VEVENT`'s lines for an "all events" edit: refreshes the
/// summary/location/notes right after `BEGIN:VEVENT`, shifts `DTSTART`/`DTEND`
/// by `shift` in place (keeping their form), and leaves the `RRULE`, any
/// nested `VALARM`, and other properties untouched.
fn rewrite_master_vevent(
    component: &[String],
    shift: SeriesShift,
    draft: &EventDraft,
) -> Vec<String> {
    let mut out = Vec::new();
    // VALARM (and other nested components) carry their own DTSTART etc.; only
    // the VEVENT's own depth-0 properties are the event's.
    let mut nested_depth = 0usize;
    for (index, line) in component.iter().enumerate() {
        if index == 0 {
            out.push(line.clone());
            out.push(format!("DTSTAMP:{}", caldav_timestamp(Local::now())));
            out.push(format!("SUMMARY:{}", escape_ics_text(&draft.title)));
            if let Some(location) = &draft.location {
                out.push(format!("LOCATION:{}", escape_ics_text(location)));
            }
            if let Some(notes) = &draft.notes {
                out.push(format!("DESCRIPTION:{}", escape_ics_text(notes)));
            }
            continue;
        }
        if is_component_keyword(line, "BEGIN") {
            nested_depth += 1;
            out.push(line.clone());
            continue;
        }
        if is_component_boundary(line, "END", "VEVENT") {
            out.push(line.clone());
            continue;
        }
        if is_component_keyword(line, "END") {
            nested_depth = nested_depth.saturating_sub(1);
            out.push(line.clone());
            continue;
        }
        if nested_depth == 0
            && let Some(name) = property_name(line)
        {
            let name = name.to_ascii_uppercase();
            // Re-added fresh above, so drop the originals.
            if matches!(
                name.as_str(),
                "DTSTAMP" | "SUMMARY" | "LOCATION" | "DESCRIPTION"
            ) {
                continue;
            }
            if (name == "DTSTART" || name == "DTEND")
                && !shift.is_zero()
                && let Some((key, value)) = line.split_once(':')
            {
                out.push(format!("{key}:{}", shift_ics_value(value, shift)));
                continue;
            }
        }
        out.push(line.clone());
    }
    out
}

/// Shifts an iCalendar date or date-time property value by `shift`, preserving
/// its form. A UTC value (`YYYYMMDDTHHMMSSZ`) names an instant and moves by the
/// exact delta; a zoned or floating one (`YYYYMMDDTHHMMSS`) and a bare date
/// (`YYYYMMDD`) read as wall-clock time and move by the wall-clock delta, so a
/// move across a DST change keeps the hour it was dragged to instead of landing
/// an hour off — or, for a date, on the same day. An unparseable value is left
/// untouched.
fn shift_ics_value(value: &str, shift: SeriesShift) -> String {
    if let Some(naive) = value.strip_suffix('Z') {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(naive, "%Y%m%dT%H%M%S") {
            return format!("{}Z", (dt + shift.exact).format("%Y%m%dT%H%M%S"));
        }
    } else if value.contains('T') {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S") {
            return (dt + shift.wall_clock).format("%Y%m%dT%H%M%S").to_string();
        }
    } else if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y%m%d") {
        // Between two local midnights the wall-clock delta is a whole number
        // of days; the exact one is a day give or take an hour, which a
        // NaiveDate addition would truncate to the wrong count.
        let days = chrono::Duration::days(shift.wall_clock.num_days());
        return (date + days).format("%Y%m%d").to_string();
    }
    value.to_string()
}

/// Excludes one occurrence of a series from its resource: adds an `EXDATE` for
/// `recurrence_id` to the master `VEVENT` and drops any override `VEVENT` that
/// redefined that same instance, so the occurrence disappears entirely.
fn exclude_recurrence_instance(ics: &str, recurrence_id: &str) -> Result<String, String> {
    let (_, recurrence_value) = split_recurrence_id(recurrence_id);
    let lines = unfold_ics(ics);

    let mut result = Vec::new();
    let mut component: Vec<String> = Vec::new();
    let mut in_event = false;
    let mut excluded = false;
    for line in lines {
        if is_component_boundary(&line, "BEGIN", "VEVENT") {
            in_event = true;
            component.clear();
        }
        if in_event {
            component.push(line.clone());
        } else {
            result.push(line.clone());
        }
        if is_component_boundary(&line, "END", "VEVENT") {
            in_event = false;
            let this_recurrence_id = component.iter().find_map(|component_line| {
                property_name(component_line)
                    .is_some_and(|name| name.eq_ignore_ascii_case("RECURRENCE-ID"))
                    .then(|| component_line.split_once(':').map(|(_, value)| value))
                    .flatten()
            });
            match this_recurrence_id {
                // An override that redefined the cancelled instance — drop it.
                Some(value) if value == recurrence_value => {
                    excluded = true;
                    component.clear();
                }
                // An override for some other instance — leave it untouched.
                Some(_) => result.append(&mut component),
                // The series master — record the exclusion on it.
                None => {
                    excluded = true;
                    let end = component.len() - 1;
                    component.insert(end, recurrence_property_line("EXDATE", recurrence_id));
                    result.append(&mut component);
                }
            }
        }
    }

    if !excluded {
        return Err("Could not find the recurring event to exclude".to_string());
    }
    Ok(result.join("\r\n") + "\r\n")
}

/// Formats `recurrence_id` as the value of property `keyword` (`RECURRENCE-ID`
/// or `EXDATE`), preserving the TZID / `VALUE=DATE` / bare-UTC form of the
/// series' `DTSTART` so it names the same instant.
fn recurrence_property_line(keyword: &str, recurrence_id: &str) -> String {
    let (tzid, value) = split_recurrence_id(recurrence_id);
    match tzid {
        Some(tzid) => format!("{keyword};TZID={tzid}:{value}"),
        None if value.len() == 8 && value.chars().all(|character| character.is_ascii_digit()) => {
            format!("{keyword};VALUE=DATE:{value}")
        }
        None => format!("{keyword}:{value}"),
    }
}

fn recurrence_exception_lines(uid: &str, recurrence_id: &str, draft: &EventDraft) -> Vec<String> {
    let (start_key, start_value, end_key, end_value) = event_time_fields(draft);
    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        recurrence_property_line("RECURRENCE-ID", recurrence_id),
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
    if let Some(freq) = draft.recurrence {
        lines.push(format!("RRULE:{}", freq.to_rrule()));
    }
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
    split_content_line(line).map(|(head, _)| head.split(';').next().unwrap_or(head))
}

/// A content line's name-and-parameters and its value, split at the first
/// colon outside double quotes. RFC 5545 quotes a parameter value that holds a
/// colon (`CN="Smith: Jo"`), and the value is free to hold more — a `mailto:`
/// address always does — so neither the first nor the last colon will do.
fn split_content_line(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some((&line[..index], &line[index + 1..])),
            _ => {}
        }
    }
    None
}

/// The `;`-separated pieces of a content line's head — the property name, then
/// each parameter — honouring quotes the way [`split_content_line`] does.
fn split_parameters(head: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (index, character) in head.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&head[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&head[start..]);
    parts
}

/// The address in a `CAL-ADDRESS` value: `mailto:` in whatever case the server
/// wrote it, then the email. A bare address is passed through as it is.
fn mailto_address(value: &str) -> &str {
    let raw = value.trim();
    match raw.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => raw[7..].trim(),
        _ => raw,
    }
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

    fn credentials_for(base_url: &str) -> Credentials {
        Credentials {
            base_url: base_url.to_string(),
            username: "person@example.com".to_string(),
            password: "secret".to_string(),
        }
    }

    #[test]
    fn an_icloud_account_is_recognized_by_its_url() {
        assert!(is_icloud(&credentials_for("https://caldav.icloud.com")));
        assert!(is_icloud(&credentials_for(
            "https://p42-caldav.icloud.com/123456/calendars/"
        )));
    }

    #[test]
    fn another_providers_account_is_not_icloud() {
        assert!(!is_icloud(&credentials_for("https://caldav.fastmail.com")));
        assert!(!is_icloud(&credentials_for(
            "https://cloud.example.com/dav"
        )));
        // Not a suffix match on the bare string: this is somebody else's host.
        assert!(!is_icloud(&credentials_for("https://evil-icloud.com")));
    }

    #[test]
    fn a_rejected_credential_on_another_provider_is_not_sent_to_apple() {
        let message = http_error(401, "", false);
        assert!(
            !message.contains("account.apple.com") && !message.contains("Apple ID"),
            "a Fastmail or Nextcloud 401 has nothing to do with Apple: {message}"
        );
        assert!(
            message.contains("401"),
            "it still has to say the credential was rejected: {message}"
        );
    }

    #[test]
    fn a_rejected_icloud_credential_says_to_generate_a_new_app_specific_password() {
        let message = http_error(401, "", true);
        assert!(
            message.contains("app-specific password"),
            "a 401 should name the fix: {message}"
        );
    }

    #[test]
    fn a_server_error_does_not_blame_the_saved_password() {
        let message = http_error(503, "Service Unavailable", true);
        assert!(
            !message.contains("app-specific password"),
            "only a 401 means the credential is dead: {message}"
        );
        assert!(
            message.contains("503") && message.contains("Service Unavailable"),
            "the status and body still have to survive: {message}"
        );
    }

    #[test]
    fn canonical_base_url_normalizes_equivalent_spellings() {
        for spelling in ["https://Host.Example.com/", "https://host.example.com"] {
            assert_eq!(
                canonical_base_url(spelling).as_deref(),
                Ok("https://host.example.com")
            );
        }
        assert_eq!(
            canonical_base_url(" https://host.example.com/dav/ ").as_deref(),
            Ok("https://host.example.com/dav")
        );
    }

    #[test]
    fn canonical_base_url_rejects_non_http_schemes() {
        assert!(canonical_base_url("ftp://host.example.com").is_err());
        assert!(canonical_base_url("not a url").is_err());
    }

    #[test]
    fn absolute_url_resolves_relative_hrefs_against_the_base() {
        assert_eq!(
            absolute_url("https://host.example.com/dav", "/cal/home/").as_deref(),
            Ok("https://host.example.com/cal/home/")
        );
    }

    #[test]
    fn absolute_url_keeps_same_origin_absolute_hrefs() {
        assert_eq!(
            absolute_url(
                "https://host.example.com/dav",
                "https://host.example.com/cal/"
            )
            .as_deref(),
            Ok("https://host.example.com/cal/")
        );
    }

    #[test]
    fn absolute_url_rejects_cross_origin_hrefs() {
        assert!(absolute_url("https://host.example.com/dav", "https://evil.example.net/").is_err());
    }

    #[test]
    fn absolute_url_rejects_downgrade_to_http_on_the_same_host() {
        assert!(absolute_url("https://host.example.com/dav", "http://host.example.com/").is_err());
    }

    #[test]
    fn absolute_url_allows_icloud_partition_hosts() {
        assert_eq!(
            absolute_url(
                "https://caldav.icloud.com",
                "https://p42-caldav.icloud.com/123456/principal/"
            )
            .as_deref(),
            Ok("https://p42-caldav.icloud.com/123456/principal/")
        );
    }

    #[test]
    fn absolute_url_only_trusts_icloud_hosts_from_an_icloud_base() {
        assert!(
            absolute_url(
                "https://host.example.com/dav",
                "https://p42-caldav.icloud.com/123456/principal/"
            )
            .is_err()
        );
        assert!(absolute_url("https://caldav.icloud.com", "https://evil-icloud.com/").is_err());
    }

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
    fn xml_entities_are_decoded_once_each() {
        // `&amp;lt;` is the four characters `&lt;`, not a `<`: decoding `&amp;`
        // first and then decoding again is what turned one into the other.
        assert_eq!(xml_unescape("a &amp;lt; b"), "a &lt; b");
        assert_eq!(
            xml_unescape("Tom &amp; Jerry &lt;3 &quot;hi&quot; &apos;x&apos; &gt;"),
            "Tom & Jerry <3 \"hi\" 'x' >"
        );
    }

    #[test]
    fn numeric_xml_entities_are_decoded_too() {
        // Servers write an apostrophe as &#39; at least as often as &apos;.
        assert_eq!(xml_unescape("Ian&#39;s &#x43;alendar"), "Ian's Calendar");
    }

    #[test]
    fn text_that_is_not_an_entity_is_left_alone() {
        assert_eq!(
            xml_unescape("R&D; &nope; &#zz; 100% &"),
            "R&D; &nope; &#zz; 100% &"
        );
    }

    #[test]
    fn multistatus_responses_are_found_whatever_namespace_prefix_the_server_picked() {
        // Fastmail writes `D:`, Nextcloud `d:`, iCloud none — and a server is
        // free to pick anything. Missing every response here is what would let
        // the sync prune a whole calendar as "gone".
        for prefix in ["ns0:", "A:", "dav:"] {
            let xml = format!(
                "<{prefix}multistatus xmlns:{}=\"DAV:\"><{prefix}response><{prefix}href>/cal/1.ics</{prefix}href>\
                 </{prefix}response><{prefix}response><{prefix}href>/cal/2.ics</{prefix}href></{prefix}response>\
                 </{prefix}multistatus>",
                prefix.trim_end_matches(':')
            );

            let responses = multistatus_responses(&xml);

            assert_eq!(responses.len(), 2, "prefix {prefix:?}: {xml}");
            assert_eq!(
                child_text(&responses[1], "href").as_deref(),
                Some("/cal/2.ics")
            );
        }
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
    fn parse_events_collects_every_attendee_with_name_and_status() {
        // Covers in one fixture: repeated ATTENDEE lines (which the
        // single-valued property map would otherwise collapse to the last), a
        // CN display name, PARTSTAT mapping, an uppercase MAILTO:, a duplicate
        // address, and an ATTENDEE belonging to a VALARM rather than the event.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nSUMMARY:Sync\r\n\
DTSTART:20260709T140000Z\r\nDTEND:20260709T143000Z\r\n\
ATTENDEE;CN=Ada Lovelace;PARTSTAT=ACCEPTED:mailto:ada@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:MAILTO:bob@example.com\r\n\
ATTENDEE;CN=Ada L;PARTSTAT=DECLINED:mailto:ADA@example.com\r\n\
BEGIN:VALARM\r\nTRIGGER:-PT10M\r\nATTENDEE:mailto:alarm@example.com\r\nEND:VALARM\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_resource("/calendars/work/x.ics", ics).0;
        assert_eq!(events.len(), 1);
        let attendees = &events[0].attendees;
        assert_eq!(attendees.len(), 2, "{attendees:?}");
        assert_eq!(attendees[0].email, "ada@example.com");
        assert_eq!(attendees[0].name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(attendees[0].status.as_deref(), Some("accepted"));
        assert_eq!(attendees[1].email, "bob@example.com");
        assert_eq!(attendees[1].name, None);
        assert_eq!(attendees[1].status.as_deref(), Some("pending"));
    }

    #[test]
    fn attendees_without_a_usable_address_are_dropped() {
        assert!(parse_ics_attendee("ATTENDEE", "urn:uuid:not-an-address").is_none());
        assert!(parse_ics_attendee("ATTENDEE", "mailto:").is_none());
        let bare = parse_ics_attendee("ATTENDEE", "someone@example.com")
            .expect("a bare address without the mailto: scheme is still usable");
        assert_eq!(bare.email, "someone@example.com");
    }

    /// An invitation with two guests, as a server writes it: the address is a
    /// `mailto:` URI, and PARTSTAT sits among other parameters rather than last.
    const INVITATION: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:i\r\n\
        ATTENDEE;CN=Ian;PARTSTAT=NEEDS-ACTION;ROLE=REQ-PARTICIPANT:mailto:ian@example.com\r\n\
        ATTENDEE;CN=Sam;PARTSTAT=ACCEPTED:mailto:sam@example.com\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn replying_to_an_invitation_replaces_partstat_and_keeps_the_mailto_address() {
        let replied = reply_to_invitation(INVITATION, "ian@example.com", "ACCEPTED").unwrap();

        assert!(
            replied.contains(
                "ATTENDEE;CN=Ian;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:ian@example.com\r\n"
            ),
            "the reply must stay a parameter and the address must stay a URI:\n{replied}"
        );
        // The other guest's line is not ours to touch.
        assert!(replied.contains("ATTENDEE;CN=Sam;PARTSTAT=ACCEPTED:mailto:sam@example.com\r\n"));
    }

    #[test]
    fn replying_adds_a_partstat_to_an_attendee_line_that_had_none() {
        let ics = "BEGIN:VEVENT\r\nATTENDEE;CN=Ian:mailto:ian@example.com\r\nEND:VEVENT\r\n";

        let replied = reply_to_invitation(ics, "ian@example.com", "DECLINED").unwrap();

        assert!(
            replied.contains("ATTENDEE;CN=Ian;PARTSTAT=DECLINED:mailto:ian@example.com\r\n"),
            "{replied}"
        );
    }

    #[test]
    fn a_quoted_parameter_holding_a_colon_does_not_split_the_attendee_line() {
        // RFC 5545 quotes a parameter value that contains a colon, so the
        // name/value separator is the first colon *outside* the quotes.
        let ics = "BEGIN:VEVENT\r\nATTENDEE;CN=\"Ian: Work\";PARTSTAT=NEEDS-ACTION:mailto:ian@example.com\r\nEND:VEVENT\r\n";

        let replied = reply_to_invitation(ics, "ian@example.com", "TENTATIVE").unwrap();

        assert!(
            replied.contains(
                "ATTENDEE;CN=\"Ian: Work\";PARTSTAT=TENTATIVE:mailto:ian@example.com\r\n"
            ),
            "{replied}"
        );
    }

    #[test]
    fn the_mailto_scheme_matches_regardless_of_case() {
        let ics = "BEGIN:VEVENT\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:MAILTO:Ian@Example.com\r\nEND:VEVENT\r\n";

        let replied = reply_to_invitation(ics, "ian@example.com", "ACCEPTED").unwrap();

        assert!(
            replied.contains("ATTENDEE;PARTSTAT=ACCEPTED:MAILTO:Ian@Example.com\r\n"),
            "{replied}"
        );
    }

    #[test]
    fn replying_when_you_are_not_a_guest_is_refused_rather_than_written() {
        assert!(reply_to_invitation(INVITATION, "nobody@example.com", "ACCEPTED").is_err());
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

        let events = parse_resource("/99509935/calendars/farren/event.ics", ics).0;

        assert_eq!(events.len(), 2);
        assert_ne!(events[0].href, events[1].href);
        assert!(events[0].href.contains("20260709T183000Z"));
        assert!(events[1].href.contains("20260716T183000Z"));
    }

    /// A multistatus body holding one response per `(href, ics)` pair.
    fn multistatus(resources: &[(&str, &str)]) -> String {
        let responses = resources
            .iter()
            .map(|(href, ics)| {
                format!(
                    "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
                     <C:calendar-data>{}</C:calendar-data>\
                     </D:prop></D:propstat></D:response>",
                    ics.replace('&', "&amp;").replace('<', "&lt;")
                )
            })
            .collect::<String>();
        format!(
            "<D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">{responses}</D:multistatus>"
        )
    }

    fn readable_ics(uid: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:Readable\r\n\
             DTSTART:20260709T140000Z\r\nDTEND:20260709T143000Z\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// A `VEVENT` whose `DTSTART` the parser cannot read.
    fn unreadable_ics(uid: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:Unreadable\r\n\
             DTSTART;VALUE=PERIOD:nonsense\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn an_unreadable_resource_is_still_reported_as_present_on_the_server() {
        // Pruning derived from the parsed events alone would delete this event
        // from the cache, and every later sync would delete it again.
        let xml = multistatus(&[
            ("/calendars/work/good.ics", &readable_ics("good")),
            ("/calendars/work/bad.ics", &unreadable_ics("bad")),
        ]);

        let synced = reconcile_calendar_query(&xml);

        assert_eq!(
            synced.events.iter().map(|e| &e.href).collect::<Vec<_>>(),
            vec!["/calendars/work/good.ics"]
        );
        assert_eq!(synced.unreadable, vec!["/calendars/work/bad.ics"]);
        assert!(synced.prunable);
    }

    #[test]
    fn one_unreadable_instance_protects_every_instance_of_its_resource() {
        // The id of a cached instance comes from the DTSTART that just failed
        // to parse, so the whole resource has to be spared.
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\nSUMMARY:Series\r\nDTSTART:20260709T183000Z\r\nDTEND:20260709T193000Z\r\n\
RECURRENCE-ID:20260709T183000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nSUMMARY:Series\r\nDTSTART;VALUE=PERIOD:nonsense\r\n\
RECURRENCE-ID:20260716T183000Z\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let xml = multistatus(&[("/calendars/work/series.ics", ics)]);

        let synced = reconcile_calendar_query(&xml);

        assert_eq!(synced.events.len(), 1);
        assert_eq!(synced.unreadable, vec!["/calendars/work/series.ics"]);
    }

    #[test]
    fn a_fully_read_resource_needs_no_protection() {
        let xml = multistatus(&[("/calendars/work/good.ics", &readable_ics("good"))]);

        let synced = reconcile_calendar_query(&xml);

        assert_eq!(synced.events.len(), 1);
        assert!(synced.unreadable.is_empty());
        assert!(synced.prunable);
    }

    #[test]
    fn a_resource_the_server_sent_no_calendar_data_for_is_protected() {
        let xml = "<D:multistatus xmlns:D=\"DAV:\"><D:response>\
                   <D:href>/calendars/work/opaque.ics</D:href>\
                   <D:status>HTTP/1.1 403 Forbidden</D:status>\
                   </D:response></D:multistatus>";

        let synced = reconcile_calendar_query(xml);

        assert!(synced.events.is_empty());
        assert_eq!(synced.unreadable, vec!["/calendars/work/opaque.ics"]);
    }

    #[test]
    fn a_response_without_an_href_makes_the_whole_query_unsafe_to_prune_from() {
        // Nothing ties this response to a resource, so we cannot say what the
        // server still has — deleting anything on that basis is a guess.
        let xml = "<D:multistatus xmlns:D=\"DAV:\"><D:response>\
                   <D:propstat><D:prop><C:calendar-data/></D:prop></D:propstat>\
                   </D:response></D:multistatus>";

        let synced = reconcile_calendar_query(xml);

        assert!(!synced.prunable);
    }

    #[test]
    fn ics_durations_parse_days_weeks_and_time_parts() {
        let cases = [
            ("P1D", 1, 0),
            ("P2W", 14, 0),
            ("PT1H30M", 0, 5400),
            ("PT45S", 0, 45),
            ("P1DT12H", 1, 43200),
            ("+P3D", 3, 0),
            ("-PT15M", 0, -900),
        ];
        for (value, days, seconds) in cases {
            let duration =
                parse_ics_duration(value).unwrap_or_else(|| panic!("{value} should parse"));
            assert_eq!(duration.days, days, "{value} days");
            assert_eq!(duration.time.num_seconds(), seconds, "{value} time");
        }
    }

    #[test]
    fn a_malformed_duration_is_not_read_as_zero() {
        for value in ["", "P", "PT", "1D", "PX", "P1D5", "PT1Q"] {
            assert!(
                parse_ics_duration(value).is_none(),
                "{value} should not parse"
            );
        }
    }

    #[test]
    fn a_timed_event_uses_its_duration_instead_of_an_invented_hour() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Standup\r\n\
DTSTART:20260709T140000Z\r\nDURATION:PT30M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_resource("/cal/duration.ics", ics).0;

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].draft.end.with_timezone(&chrono::Utc),
            chrono::Utc.with_ymd_and_hms(2026, 7, 9, 14, 30, 0).unwrap()
        );
    }

    #[test]
    fn an_all_day_event_uses_its_duration_in_whole_days() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Conference\r\n\
DTSTART;VALUE=DATE:20260709\r\nDURATION:P3D\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_resource("/cal/allday.ics", ics).0;

        assert_eq!(events.len(), 1);
        assert!(events[0].draft.all_day);
        assert_eq!(
            events[0].draft.end,
            crate::date_util::local_day_start(NaiveDate::from_ymd_opt(2026, 7, 12).unwrap())
        );
    }

    #[test]
    fn an_all_day_event_ends_on_the_next_civil_date_across_a_dst_jump() {
        // US clocks spring forward at 02:00 on 2026-03-08, so that civil day is
        // 23 hours long. Adding 24 elapsed hours would end the event at 01:00
        // on the 9th instead of at its midnight.
        let tz = chrono_tz::America::New_York;
        let start =
            crate::date_util::day_start_in(&tz, NaiveDate::from_ymd_opt(2026, 3, 8).unwrap());
        let one_day = IcsDuration {
            days: 1,
            time: chrono::Duration::zero(),
        };

        let end = end_from_duration(&start, true, &one_day);

        assert_eq!(
            end.naive_local(),
            NaiveDate::from_ymd_opt(2026, 3, 9)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        );
        assert_eq!((end - start).num_hours(), 23);
    }

    #[test]
    fn a_timed_events_duration_stays_exact() {
        let tz = chrono_tz::America::New_York;
        let start = tz.with_ymd_and_hms(2026, 7, 9, 9, 0, 0).unwrap();
        let ninety_minutes = IcsDuration {
            days: 0,
            time: chrono::Duration::minutes(90),
        };

        let end = end_from_duration(&start, false, &ninety_minutes);

        assert_eq!(end, tz.with_ymd_and_hms(2026, 7, 9, 10, 30, 0).unwrap());
    }

    #[test]
    fn an_unreadable_dtend_is_not_replaced_with_an_invented_one() {
        // The server did send an end; failing to read it must drop the event
        // into the unreadable set rather than silently inventing an hour.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Broken\r\n\
DTSTART:20260709T140000Z\r\nDTEND;VALUE=PERIOD:nonsense\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let (events, complete) = parse_resource("/cal/broken.ics", ics);

        assert!(events.is_empty());
        assert!(!complete);
    }

    #[test]
    fn an_unreadable_duration_is_not_replaced_with_an_invented_one() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Broken\r\n\
DTSTART:20260709T140000Z\r\nDURATION:sometime\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let (events, complete) = parse_resource("/cal/broken.ics", ics);

        assert!(events.is_empty());
        assert!(!complete);
    }

    #[test]
    fn an_all_day_event_with_no_end_at_all_still_spans_one_civil_day() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Holiday\r\n\
DTSTART;VALUE=DATE:20260709\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_resource("/cal/holiday.ics", ics).0;

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].draft.end,
            crate::date_util::local_day_start(NaiveDate::from_ymd_opt(2026, 7, 10).unwrap())
        );
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

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn a_zoned_time_the_clocks_skipped_is_read_with_the_offset_before_the_gap() {
        // New York sprang forward at 2 AM on 2026-03-08, so 2:30 never
        // happened. RFC 5545 reads it at the old offset — 2:30 EST, which is
        // 3:30 EDT — and so do Apple and Google; dropping the event as
        // unreadable would leave a stale cached copy in its place.
        let property = IcsProperty {
            value: "20260308T023000".to_string(),
            tzid: Some("America/New_York".to_string()),
        };

        let (datetime, _) = parse_ics_datetime(&property).expect("a real instant");

        assert_eq!(datetime.with_timezone(&chrono::Utc), utc(2026, 3, 8, 7, 30));
    }

    #[test]
    fn a_floating_time_the_clocks_skipped_is_read_with_the_offset_before_the_gap() {
        let property = IcsProperty {
            value: "20260308T023000".to_string(),
            tzid: None,
        };

        let (datetime, _) = parse_ics_datetime_in(&property, &chrono_tz::America::New_York)
            .expect("a real instant");

        assert_eq!(datetime.with_timezone(&chrono::Utc), utc(2026, 3, 8, 7, 30));
    }

    #[test]
    fn a_floating_time_the_clocks_repeated_takes_its_first_occurrence() {
        // 1:30 AM on 2026-11-01 happens twice in New York; the TZID branch
        // already takes the earlier one, and a floating value should agree.
        let property = IcsProperty {
            value: "20261101T013000".to_string(),
            tzid: None,
        };

        let (datetime, _) = parse_ics_datetime_in(&property, &chrono_tz::America::New_York)
            .expect("a real instant");

        assert_eq!(
            datetime.with_timezone(&chrono::Utc),
            utc(2026, 11, 1, 5, 30)
        );
    }

    #[test]
    fn editing_a_one_off_replaces_a_duration_with_the_new_end() {
        // RFC 5545 allows DTEND or DURATION, never both; a resource authored
        // with a DURATION must come back with just the DTEND the draft gives it.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART:20260701T140000Z\r\nDURATION:PT1H\r\nSUMMARY:Old\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = replace_event_fields(ics, &series_draft("New")).unwrap();

        assert!(!updated.contains("DURATION"), "{updated}");
        assert!(updated.contains("DTEND:"), "{updated}");
    }

    #[test]
    fn editing_an_unexpanded_series_master_is_refused_rather_than_reanchored() {
        // A server that ignored <C:expand> hands back the master itself. Its
        // DTSTART is the whole series' anchor: rewriting it to the occurrence
        // that was clicked, in UTC, would move every occurrence and lose the
        // zone the rule repeats in.
        for rule in ["RRULE:FREQ=WEEKLY", "RDATE:20260715T130000Z"] {
            let ics = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m\r\nDTSTART;TZID=America/New_York:20260701T090000\r\nDTEND;TZID=America/New_York:20260701T093000\r\n{rule}\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );

            assert!(
                replace_event_fields(&ics, &series_draft("Renamed")).is_err(),
                "{rule} should have stopped the edit"
            );
        }
    }

    #[test]
    fn an_alarm_trigger_does_not_make_a_one_off_look_recurring() {
        // VALARM carries its own properties; only the VEVENT's own RRULE counts.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260701T140000Z\r\nDTEND:20260701T143000Z\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nRRULE:FREQ=DAILY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        assert!(replace_event_fields(ics, &series_draft("Renamed")).is_ok());
    }

    #[test]
    fn replacing_event_fields_preserves_unedited_ics_properties() {
        // A one-off: a master with an RRULE is refused outright (see
        // `editing_an_unexpanded_series_master_is_refused_rather_than_reanchored`).
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc\r\nSUMMARY:Old title\r\nDTSTART;TZID=America/New_York:20260709T090000\r\nDTEND;TZID=America/New_York:20260709T100000\r\nCATEGORIES:Work\r\nATTENDEE:mailto:friend@example.com\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nLOCATION:Old location\r\nDESCRIPTION:Old notes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local::now();
        let draft = EventDraft {
            title: "New title".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let updated = replace_event_fields(ics, &draft).unwrap();

        assert!(updated.contains("UID:abc"));
        assert!(updated.contains("CATEGORIES:Work"));
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
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let updated = replace_recurrence_instance(ics, "20260709T140000Z", &draft).unwrap();

        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert!(updated.contains("UID:weekly-standup"));
        assert!(updated.contains("RECURRENCE-ID:20260709T140000Z"));
        assert!(updated.contains("SUMMARY:Moved standup"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 2);
    }

    #[test]
    fn parse_events_keeps_recurrence_id_tzid_in_the_instance_identity() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:Standup
DTSTART;TZID=America/New_York:20260709T090000
DTEND;TZID=America/New_York:20260709T093000
RECURRENCE-ID;TZID=America/New_York:20260709T090000
END:VEVENT
BEGIN:VEVENT
SUMMARY:Standup
DTSTART;TZID=America/New_York:20260716T090000
DTEND;TZID=America/New_York:20260716T093000
RECURRENCE-ID;TZID=America/New_York:20260716T090000
END:VEVENT
END:VCALENDAR"#;

        let events = parse_resource("/cal/standup.ics", ics).0;

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].href,
            "/cal/standup.ics#TZID=America/New_York:20260709T090000"
        );
        assert_eq!(
            events[1].href,
            "/cal/standup.ics#TZID=America/New_York:20260716T090000"
        );
    }

    #[test]
    fn replacing_recurrence_instance_reproduces_the_tzid_form() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20260709T090000\r\nDTEND;TZID=America/New_York:20260709T093000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local
            .with_ymd_and_hms(2026, 7, 9, 15, 0, 0)
            .single()
            .unwrap();
        let draft = EventDraft {
            title: "Moved standup".to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let updated =
            replace_recurrence_instance(ics, "TZID=America/New_York:20260709T090000", &draft)
                .unwrap();

        assert!(updated.contains("RECURRENCE-ID;TZID=America/New_York:20260709T090000"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 2);
    }

    #[test]
    fn replacing_recurrence_instance_replaces_an_existing_tzid_exception() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20260709T090000\r\nDTEND;TZID=America/New_York:20260709T093000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nRECURRENCE-ID;TZID=America/New_York:20260709T090000\r\nSUMMARY:Old exception\r\nDTSTART;TZID=America/New_York:20260709T100000\r\nDTEND;TZID=America/New_York:20260709T103000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local
            .with_ymd_and_hms(2026, 7, 9, 15, 0, 0)
            .single()
            .unwrap();
        let draft = EventDraft {
            title: "New exception".to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let updated =
            replace_recurrence_instance(ics, "TZID=America/New_York:20260709T090000", &draft)
                .unwrap();

        assert!(!updated.contains("Old exception"));
        assert!(updated.contains("SUMMARY:New exception"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 2);
        assert_eq!(
            updated
                .matches("RECURRENCE-ID;TZID=America/New_York:20260709T090000")
                .count(),
            1
        );
    }

    #[test]
    fn all_day_recurrence_exception_still_writes_value_date() {
        let start = Local
            .with_ymd_and_hms(2026, 7, 9, 0, 0, 0)
            .single()
            .unwrap();
        let draft = EventDraft {
            title: "Holiday".to_string(),
            start,
            end: start + chrono::Duration::days(1),
            all_day: true,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let lines = recurrence_exception_lines("uid", "20260709", &draft);

        assert!(lines.contains(&"RECURRENCE-ID;VALUE=DATE:20260709".to_string()));
    }

    #[test]
    fn new_event_ics_writes_an_rrule_only_for_a_recurring_draft() {
        let start = Local
            .with_ymd_and_hms(2026, 7, 9, 9, 0, 0)
            .single()
            .unwrap();
        let mut draft = EventDraft {
            title: "Standup".to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: None,
            recurrence: Some(crate::recurrence::Frequency::Weekly),
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let ics = new_event_ics("uid-1", &draft);
        assert!(ics.contains("RRULE:FREQ=WEEKLY"));

        draft.recurrence = None;
        let ics = new_event_ics("uid-1", &draft);
        assert!(!ics.contains("RRULE"));
    }

    #[test]
    fn excluding_an_instance_adds_an_exdate_and_keeps_the_series() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART:20260709T140000Z\r\nDTEND:20260709T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = exclude_recurrence_instance(ics, "20260709T140000Z").unwrap();

        assert!(updated.contains("EXDATE:20260709T140000Z"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert!(updated.contains("UID:weekly-standup"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 1);
    }

    #[test]
    fn excluding_a_tzid_instance_writes_a_tzid_exdate() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20260709T090000\r\nDTEND;TZID=America/New_York:20260709T093000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated =
            exclude_recurrence_instance(ics, "TZID=America/New_York:20260709T090000").unwrap();

        assert!(updated.contains("EXDATE;TZID=America/New_York:20260709T090000"));
    }

    #[test]
    fn excluding_an_all_day_instance_writes_a_value_date_exdate() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:holiday\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260701\r\nDTEND;VALUE=DATE:20260702\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = exclude_recurrence_instance(ics, "20260701").unwrap();

        assert!(updated.contains("EXDATE;VALUE=DATE:20260701"));
    }

    fn series_draft(title: &str) -> EventDraft {
        let start = Local
            .with_ymd_and_hms(2026, 7, 1, 9, 0, 0)
            .single()
            .unwrap();
        EventDraft {
            title: title.to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        }
    }

    /// A move that crosses no DST transition, so its exact and wall-clock
    /// deltas agree.
    fn shift_of_hours(hours: i64) -> SeriesShift {
        let delta = chrono::Duration::hours(hours);
        SeriesShift {
            exact: delta,
            wall_clock: delta,
        }
    }

    #[test]
    fn editing_all_events_shifts_the_series_start_and_end_by_the_delta() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Old title\r\nDTSTART:20260701T140000Z\r\nDTEND:20260701T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated =
            edit_master_series(ics, shift_of_hours(2), &series_draft("New title")).unwrap();

        assert!(updated.contains("DTSTART:20260701T160000Z"));
        assert!(updated.contains("DTEND:20260701T163000Z"));
        assert!(updated.contains("SUMMARY:New title"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY"));
        assert!(!updated.contains("Old title"));
    }

    #[test]
    fn editing_all_events_preserves_the_tzid_form_and_other_instances() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20260701T090000\r\nDTEND;TZID=America/New_York:20260701T093000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID;TZID=America/New_York:20260708T090000\r\nSUMMARY:Moved one\r\nDTSTART;TZID=America/New_York:20260708T100000\r\nDTEND;TZID=America/New_York:20260708T103000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = edit_master_series(ics, shift_of_hours(1), &series_draft("Renamed")).unwrap();

        // Master shifted by an hour, in the same TZID form.
        assert!(updated.contains("DTSTART;TZID=America/New_York:20260701T100000"));
        assert!(updated.contains("SUMMARY:Renamed"));
        // The already-customized instance is left exactly as it was.
        assert!(updated.contains("RECURRENCE-ID;TZID=America/New_York:20260708T090000"));
        assert!(updated.contains("SUMMARY:Moved one"));
        assert!(updated.contains("DTSTART;TZID=America/New_York:20260708T100000"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 2);
    }

    /// One day forward in New York, starting `day` March 2026 at `hour`, chosen
    /// so the move crosses the 2 AM March 8 spring-forward: 23 elapsed hours.
    fn one_day_across_spring_forward(day: u32, hour: u32) -> SeriesShift {
        let ny = chrono_tz::America::New_York;
        SeriesShift::between(
            ny.with_ymd_and_hms(2026, 3, day, hour, 0, 0).unwrap(),
            ny.with_ymd_and_hms(2026, 3, day + 1, hour, 0, 0).unwrap(),
        )
    }

    #[test]
    fn moving_an_all_day_series_a_day_across_a_dst_change_moves_it_a_whole_day() {
        // A bare date has no clock to lose an hour from: 23 elapsed hours from
        // one midnight to the next is still exactly one calendar day.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:h\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260308\r\nDTEND;VALUE=DATE:20260309\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        // Midnight on the 8th is still before the change; midnight on the 9th
        // is after it.
        let updated = edit_master_series(
            ics,
            one_day_across_spring_forward(8, 0),
            &series_draft("Holiday"),
        )
        .unwrap();

        assert!(updated.contains("DTSTART;VALUE=DATE:20260309"), "{updated}");
        assert!(updated.contains("DTEND;VALUE=DATE:20260310"), "{updated}");
    }

    #[test]
    fn moving_a_zoned_series_across_a_dst_change_keeps_its_wall_clock_time() {
        // A value in a named zone reads as wall-clock time: a 9 AM standup
        // moved a day stays a 9 AM standup, even though only 23 hours passed.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20260307T090000\r\nDTEND;TZID=America/New_York:20260307T093000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        // 9 AM Saturday the 7th is EST; 9 AM Sunday the 8th is EDT.
        let updated = edit_master_series(
            ics,
            one_day_across_spring_forward(7, 9),
            &series_draft("Standup"),
        )
        .unwrap();

        assert!(
            updated.contains("DTSTART;TZID=America/New_York:20260308T090000"),
            "{updated}"
        );
        assert!(
            updated.contains("DTEND;TZID=America/New_York:20260308T093000"),
            "{updated}"
        );
    }

    #[test]
    fn moving_a_utc_series_across_a_dst_change_moves_the_instant() {
        // Same move, but a UTC value names an instant, so it takes the exact 23
        // hours — which is the same new 9 AM, spelled in UTC.
        // 9 AM EST on the 7th is 14:00Z; 9 AM EDT on the 8th is 13:00Z.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Standup\r\nDTSTART:20260307T140000Z\r\nDTEND:20260307T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = edit_master_series(
            ics,
            one_day_across_spring_forward(7, 9),
            &series_draft("Standup"),
        )
        .unwrap();

        assert!(updated.contains("DTSTART:20260308T130000Z"), "{updated}");
        assert!(updated.contains("DTEND:20260308T133000Z"), "{updated}");
    }

    #[test]
    fn editing_all_events_adds_location_and_notes_when_the_draft_has_them() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Standup\r\nDTSTART:20260701T140000Z\r\nDTEND:20260701T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut draft = series_draft("Standup");
        draft.location = Some("Room 5".to_string());
        draft.notes = Some("Bring notes".to_string());

        let updated = edit_master_series(ics, shift_of_hours(0), &draft).unwrap();

        assert!(updated.contains("LOCATION:Room 5"));
        assert!(updated.contains("DESCRIPTION:Bring notes"));
    }

    #[test]
    fn excluding_an_instance_that_was_modified_drops_its_override_too() {
        // A series whose 2026-07-16 occurrence was already moved to a later time.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nSUMMARY:Standup\r\nDTSTART:20260709T140000Z\r\nDTEND:20260709T143000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-standup\r\nRECURRENCE-ID:20260716T140000Z\r\nSUMMARY:Moved standup\r\nDTSTART:20260716T150000Z\r\nDTEND:20260716T153000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let updated = exclude_recurrence_instance(ics, "20260716T140000Z").unwrap();

        // The override is gone and the master carries the exclusion.
        assert!(!updated.contains("Moved standup"));
        assert!(updated.contains("EXDATE:20260716T140000Z"));
        assert_eq!(updated.matches("BEGIN:VEVENT").count(), 1);
    }

    #[test]
    fn nested_alarm_description_does_not_overwrite_the_event_description() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:evt-1\r\nSUMMARY:Standup\r\nDTSTART:20260110T090000Z\r\nDTEND:20260110T093000Z\r\nDESCRIPTION:Real agenda\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_resource("/cal/1.ics", ics).0;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].draft.notes.as_deref(), Some("Real agenda"));
    }

    #[test]
    fn editing_an_event_leaves_its_nested_alarm_intact() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:evt-1\r\nSUMMARY:Standup\r\nDTSTART:20260110T090000Z\r\nDTEND:20260110T093000Z\r\nDESCRIPTION:Old agenda\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Local::now();
        let draft = EventDraft {
            title: "Standup".to_string(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            location: None,
            notes: Some("New agenda".to_string()),
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        };

        let updated = replace_event_fields(ics, &draft).unwrap();

        // The alarm and its own fields survive untouched...
        assert_eq!(updated.matches("BEGIN:VALARM").count(), 1);
        assert_eq!(updated.matches("END:VALARM").count(), 1);
        assert!(updated.contains("TRIGGER:-PT10M"));
        assert!(updated.contains("DESCRIPTION:Reminder"));
        // ...while the event's own description is replaced, not the alarm's.
        assert!(updated.contains("DESCRIPTION:New agenda"));
        assert!(!updated.contains("Old agenda"));
    }

    #[test]
    fn parses_lowercase_component_and_property_names() {
        let ics = "begin:vcalendar\r\nbegin:vevent\r\nuid:evt-2\r\nsummary:Lunch\r\ndtstart:20260110T120000Z\r\ndtend:20260110T130000Z\r\nend:vevent\r\nend:vcalendar\r\n";

        let events = parse_resource("/cal/2.ics", ics).0;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].draft.title, "Lunch");
    }
}
