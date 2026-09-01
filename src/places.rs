//! Location type-ahead: turning what someone has typed into address
//! suggestions.
//!
//! Suggestions come from two places. Locations already used in this calendar
//! answer instantly, offline, and are usually what a recurring meeting wants.
//! Behind them sits a geocoder — Photon by default, which is built for
//! type-ahead over OpenStreetMap data and needs no API key. The typed prefix is
//! all that leaves the machine, and only once it is long enough to mean
//! something; `[places] enabled = false` in `config.toml` turns that half off
//! and leaves the local half working.
//!
//! An event's location is a plain string everywhere — in storage, in iCalendar
//! `LOCATION`, in Google's `location` — so a picked suggestion is just
//! well-formed text. Nothing here changes what an event is.

use serde::Deserialize;

/// Photon's public endpoint. It speaks OpenStreetMap data, is explicitly meant
/// for autocomplete, and needs no key. A self-hosted Photon or any other
/// endpoint with the same shape can replace it in `config.toml`.
pub const DEFAULT_ENDPOINT: &str = "https://photon.komoot.io/api";

/// How many suggestions the picker offers at once. Long enough to hold the
/// right answer, short enough not to cover the dialog it drops out of.
pub const MAX_SUGGESTIONS: usize = 6;

/// The shortest prefix worth sending. One or two characters match half the
/// planet, so the results would be noise bought with a round trip.
const MIN_QUERY_LEN: usize = 3;

/// Whether `query` is worth asking the geocoder about.
pub fn should_search(query: &str) -> bool {
    query.trim().chars().count() >= MIN_QUERY_LEN
}

/// The geocoder request for `query`, with the typed text encoded rather than
/// pasted into the URL — a location can hold `&`, `#` or a space, and every one
/// of those would otherwise change which request gets made.
pub fn search_url(endpoint: &str, query: &str, limit: usize) -> String {
    let params = [
        ("q", query.trim().to_string()),
        ("limit", limit.to_string()),
        ("lang", "en".to_string()),
    ];
    match url::Url::parse_with_params(endpoint, params.iter()) {
        Ok(url) => url.to_string(),
        // A misconfigured endpoint shouldn't panic mid-keystroke; the request
        // that follows fails and the picker simply shows local history.
        Err(_) => String::new(),
    }
}

/// Reads a Photon (GeoJSON) response into the labels the picker shows.
pub fn parse_response(body: &str) -> Result<Vec<String>, String> {
    let response: Response = serde_json::from_str(body)
        .map_err(|error| format!("couldn't read the location results: {error}"))?;
    Ok(response
        .features
        .into_iter()
        .filter_map(|feature| format_place(&feature.properties))
        .collect())
}

/// What the user's own calendar knows, then what the geocoder found.
///
/// History comes first because a place already used is far likelier to be
/// meant than a fresh match, and because it is the half that is certainly
/// spelled the way this calendar spells it. Duplicates are matched without
/// regard to case, so a geocoder's "300 Webster Street" doesn't appear a
/// second time under a slightly different capitalization.
pub fn suggestions(history: Vec<String>, remote: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for candidate in history.into_iter().chain(remote) {
        let candidate = candidate.trim().to_string();
        if candidate.is_empty() {
            continue;
        }
        let key = candidate.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(candidate);
        if out.len() == limit {
            break;
        }
    }
    out
}

/// Asks the geocoder about `query`. Blocking: callers run it on a worker
/// thread, the way every other network call in Calix runs.
pub fn search(endpoint: &str, query: &str) -> Result<Vec<String>, String> {
    let url = search_url(endpoint, query, MAX_SUGGESTIONS);
    if url.is_empty() {
        return Err("the configured location endpoint isn't a usable URL".to_string());
    }
    let response = crate::http::client()?
        // Nominatim-family services ask that a client identify itself, and a
        // request that doesn't can be refused outright.
        .get(url)
        .header(
            "User-Agent",
            format!("Calix/{} (calendar)", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .map_err(|error| format!("couldn't reach the location service: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "the location service answered {}",
            response.status()
        ));
    }
    let body = response
        .text()
        .map_err(|error| format!("couldn't read the location results: {error}"))?;
    parse_response(&body)
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    #[serde(default)]
    properties: Properties,
}

/// The address fields Photon returns. Every one of them is optional: a café has
/// a name and no housenumber, a city has neither.
#[derive(Deserialize, Default)]
struct Properties {
    name: Option<String>,
    housenumber: Option<String>,
    street: Option<String>,
    city: Option<String>,
    state: Option<String>,
    postcode: Option<String>,
    country: Option<String>,
}

/// One result as a person would write the address: the place, then the street,
/// then where on earth it is. `None` when the result carries nothing to show.
fn format_place(place: &Properties) -> Option<String> {
    let street = match (trimmed(&place.housenumber), trimmed(&place.street)) {
        (Some(number), Some(street)) => Some(format!("{number} {street}")),
        (None, street) => street,
        // A house number with no street names nothing on its own.
        (Some(_), None) => None,
    };
    // State and postcode are one line of an address, not two: "California
    // 94607" is how it is written and how it is read back.
    let region = match (trimmed(&place.state), trimmed(&place.postcode)) {
        (Some(state), Some(postcode)) => Some(format!("{state} {postcode}")),
        (state, None) => state,
        (None, Some(postcode)) => Some(postcode),
    };

    let mut parts: Vec<String> = Vec::new();
    for part in [
        trimmed(&place.name),
        street,
        trimmed(&place.city),
        region,
        trimmed(&place.country),
    ]
    .into_iter()
    .flatten()
    {
        // A result whose name is its street (or its city) would otherwise say
        // it twice: "Webster Street, Webster Street, Oakland".
        if parts
            .last()
            .is_some_and(|previous| previous.eq_ignore_ascii_case(&part))
        {
            continue;
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn trimmed(field: &Option<String>) -> Option<String> {
    field
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photon(properties: &str) -> String {
        format!(
            r#"{{"type":"FeatureCollection","features":[
                {{"type":"Feature","geometry":{{"type":"Point","coordinates":[-122.2,37.8]}},
                 "properties":{properties}}}]}}"#
        )
    }

    #[test]
    fn a_prefix_too_short_to_mean_anything_is_not_sent() {
        assert!(!should_search(""));
        assert!(!should_search("  "));
        assert!(!should_search("Bl"));
        assert!(!should_search("  B "));
        assert!(should_search("Blu"));
        assert!(should_search(" Blue Bottle "));
    }

    #[test]
    fn the_search_url_encodes_what_was_typed() {
        let url = search_url(DEFAULT_ENDPOINT, "Ben & Jerry's, 5th Ave", 6);

        assert!(url.starts_with("https://photon.komoot.io/api?"));
        assert!(
            url.contains("q=Ben+%26+Jerry%27s%2C+5th+Ave"),
            "the ampersand and comma must not start a new parameter: {url}"
        );
        assert!(url.contains("limit=6"));
    }

    #[test]
    fn a_named_place_reads_as_its_name_then_its_address() {
        let body = photon(
            r#"{"name":"Blue Bottle Coffee","housenumber":"300","street":"Webster Street",
                "city":"Oakland","state":"California","postcode":"94607","country":"United States"}"#,
        );

        assert_eq!(
            parse_response(&body).unwrap(),
            vec![
                "Blue Bottle Coffee, 300 Webster Street, Oakland, California 94607, United States"
            ]
        );
    }

    #[test]
    fn a_street_address_with_no_name_starts_at_the_street() {
        let body = photon(
            r#"{"housenumber":"221B","street":"Baker Street","city":"London",
                "postcode":"NW1 6XE","country":"United Kingdom"}"#,
        );

        assert_eq!(
            parse_response(&body).unwrap(),
            vec!["221B Baker Street, London, NW1 6XE, United Kingdom"]
        );
    }

    #[test]
    fn a_result_does_not_say_the_same_thing_twice() {
        // Photon names a street result after the street it is.
        let body = photon(
            r#"{"name":"Webster Street","street":"Webster Street","city":"Oakland",
                "country":"United States"}"#,
        );

        assert_eq!(
            parse_response(&body).unwrap(),
            vec!["Webster Street, Oakland, United States"]
        );
    }

    #[test]
    fn a_city_result_is_just_where_it_is() {
        let body = photon(r#"{"name":"Oakland","state":"California","country":"United States"}"#);

        assert_eq!(
            parse_response(&body).unwrap(),
            vec!["Oakland, California, United States"]
        );
    }

    #[test]
    fn a_result_with_nothing_to_show_is_dropped_rather_than_shown_blank() {
        let body = photon(r#"{"housenumber":"300"}"#);

        assert!(parse_response(&body).unwrap().is_empty());
    }

    #[test]
    fn a_search_that_matched_nothing_is_an_empty_list_not_a_failure() {
        let body = r#"{"type":"FeatureCollection","features":[]}"#;

        assert_eq!(parse_response(body).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_response_that_is_not_json_is_reported_rather_than_panicking() {
        // A captive portal or a proxy error page reaches the parser as HTML.
        let error = parse_response("<html>502 Bad Gateway</html>").unwrap_err();

        assert!(error.contains("location results"), "{error}");
    }

    #[test]
    fn locations_already_used_here_come_before_the_geocoders() {
        let merged = suggestions(
            vec!["Suite 210".to_string()],
            vec!["Suite 210 Building B".to_string()],
            MAX_SUGGESTIONS,
        );

        assert_eq!(merged, vec!["Suite 210", "Suite 210 Building B"]);
    }

    #[test]
    fn the_same_place_from_both_halves_is_offered_once() {
        let merged = suggestions(
            vec!["300 Webster Street, Oakland".to_string()],
            vec![
                "300 webster street, oakland".to_string(),
                "301 Webster Street, Oakland".to_string(),
            ],
            MAX_SUGGESTIONS,
        );

        assert_eq!(
            merged,
            vec!["300 Webster Street, Oakland", "301 Webster Street, Oakland"],
            "the history spelling is the one that survives"
        );
    }

    #[test]
    fn the_picker_is_never_handed_more_rows_than_it_shows() {
        let remote: Vec<String> = (0..20).map(|i| format!("Place {i}")).collect();

        let merged = suggestions(vec!["Home".to_string()], remote, MAX_SUGGESTIONS);

        assert_eq!(merged.len(), MAX_SUGGESTIONS);
        assert_eq!(merged[0], "Home");
    }

    #[test]
    fn blank_history_rows_never_become_blank_suggestions() {
        let merged = suggestions(
            vec!["   ".to_string(), "Home".to_string()],
            Vec::new(),
            MAX_SUGGESTIONS,
        );

        assert_eq!(merged, vec!["Home"]);
    }
}
