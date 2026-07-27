//! The one HTTP client every provider request goes through.
//!
//! Requests run on worker threads while the GTK side polls their channel and
//! keeps the button that started them insensitive. A request with no timeout
//! therefore isn't just slow: a peer that accepts the connection and then stops
//! responding leaves Add/Sync/Save/Delete disabled, and the polling source
//! alive, for the rest of the process. Every client built here carries both a
//! connect and a whole-request deadline so that failure arrives as an ordinary
//! error the UI already knows how to report.

use oauth2::reqwest;
use std::sync::OnceLock;
use std::time::Duration;

/// Long enough for a slow handshake to a distant server, short enough that a
/// black-holed connection reports back while the user is still watching.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Covers the whole request, including reading the body. A full calendar
/// REPORT over a slow link is the longest legitimate request here, so this is
/// generous — its job is to bound the hang, not to police latency.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
static NO_REDIRECT_CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();

/// The shared client for Google and CalDAV calls. Cloning is cheap — the
/// clones share one connection pool — so callers take it by value.
pub fn client() -> Result<reqwest::blocking::Client, String> {
    CLIENT
        .get_or_init(|| build(reqwest::redirect::Policy::default()))
        .clone()
}

/// A client that refuses redirects, for the OAuth token endpoints where
/// following one would open the flow up to SSRF.
pub fn no_redirect_client() -> Result<reqwest::blocking::Client, String> {
    NO_REDIRECT_CLIENT
        .get_or_init(|| build(reqwest::redirect::Policy::none()))
        .clone()
}

fn build(redirect: reqwest::redirect::Policy) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::ClientBuilder::new()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(redirect)
        .build()
        .map_err(|e| format!("couldn't start an HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shared_clients_build_with_their_timeouts_applied() {
        // A builder misconfiguration would otherwise only surface as a failed
        // sync on a machine with no network to blame it on.
        assert!(client().is_ok());
        assert!(no_redirect_client().is_ok());
    }
}
