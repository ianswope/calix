use crate::config::GoogleConfig;
use oauth2::basic::BasicClient;
use oauth2::reqwest;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use url::Url;

const KEYRING_SERVICE: &str = "com.ianswope.Calix";
const KEYRING_USERNAME: &str = "google-refresh-token";
const SCOPE: &str = "https://www.googleapis.com/auth/calendar";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://www.googleapis.com/oauth2/v3/token";

#[derive(Debug)]
pub enum AuthError {
    Io(std::io::Error),
    Oauth(String),
    MissingRedirectCode,
    StateMismatch,
    NoRefreshToken,
    Keyring(keyring::Error),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Io(e) => write!(f, "network error: {e}"),
            AuthError::Oauth(e) => write!(f, "Google rejected the request: {e}"),
            AuthError::MissingRedirectCode => {
                write!(f, "Google's redirect didn't include an authorization code")
            }
            AuthError::StateMismatch => write!(f, "OAuth state mismatch (possible CSRF)"),
            AuthError::NoRefreshToken => {
                write!(f, "Google didn't return a refresh token — try disconnecting and reconnecting")
            }
            AuthError::Keyring(e) => write!(f, "couldn't access the system keyring: {e}"),
        }
    }
}

fn keyring_entry() -> Result<keyring::Entry, AuthError> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME).map_err(AuthError::Keyring)
}

pub fn has_saved_account() -> bool {
    keyring_entry().and_then(|e| e.get_password().map_err(AuthError::Keyring)).is_ok()
}

pub fn sign_out() {
    if let Ok(entry) = keyring_entry() {
        let _ = entry.delete_credential();
    }
}

/// Runs the full interactive OAuth flow: opens the user's browser, waits for
/// the redirect on a one-shot local loopback listener, exchanges the code
/// for tokens, and saves the refresh token to the system keyring.
///
/// This blocks the calling thread on network I/O and the user's browser
/// interaction — always call it from a background thread, never the GTK
/// main thread.
pub fn sign_in(config: &GoogleConfig) -> Result<(), AuthError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(AuthError::Io)?;
    let port = listener.local_addr().map_err(AuthError::Io)?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let client = BasicClient::new(ClientId::new(config.client_id.clone()))
        .set_client_secret(ClientSecret::new(config.client_secret.clone()))
        .set_auth_uri(AuthUrl::new(AUTH_URL.to_string()).expect("AUTH_URL is a valid URL"))
        .set_token_uri(TokenUrl::new(TOKEN_URL.to_string()).expect("TOKEN_URL is a valid URL"))
        .set_redirect_uri(RedirectUrl::new(redirect_uri).expect("loopback URL is always valid"));

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(SCOPE.to_string()))
        .set_pkce_challenge(pkce_challenge)
        // offline + consent ensures Google actually issues a refresh token,
        // not just an access token — without these it only does on first
        // ever consent, which breaks re-connecting after a sign-out.
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .url();

    open_in_browser(auth_url.as_str());

    let (code, state) = receive_redirect(&listener)?;
    if state.secret() != csrf_token.secret() {
        return Err(AuthError::StateMismatch);
    }

    let http_client = http_client()?;
    let token = client
        .exchange_code(code)
        .set_pkce_verifier(pkce_verifier)
        .request(&http_client)
        .map_err(|e| AuthError::Oauth(e.to_string()))?;

    let refresh_token = token.refresh_token().ok_or(AuthError::NoRefreshToken)?;
    keyring_entry()?
        .set_password(refresh_token.secret())
        .map_err(AuthError::Keyring)?;

    Ok(())
}

/// Exchanges the saved refresh token for a fresh access token. Returns
/// `Ok(None)` if no account has been connected yet. Blocks on network I/O —
/// call from a background thread.
pub fn get_access_token(config: &GoogleConfig) -> Result<Option<String>, AuthError> {
    let entry = keyring_entry()?;
    let refresh_token = match entry.get_password() {
        Ok(token) => token,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => return Err(AuthError::Keyring(e)),
    };

    let client = BasicClient::new(ClientId::new(config.client_id.clone()))
        .set_client_secret(ClientSecret::new(config.client_secret.clone()))
        .set_auth_uri(AuthUrl::new(AUTH_URL.to_string()).expect("AUTH_URL is a valid URL"))
        .set_token_uri(TokenUrl::new(TOKEN_URL.to_string()).expect("TOKEN_URL is a valid URL"));

    let http_client = http_client()?;
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token))
        .request(&http_client)
        .map_err(|e| AuthError::Oauth(e.to_string()))?;

    Ok(Some(token.access_token().secret().clone()))
}

fn http_client() -> Result<reqwest::blocking::Client, AuthError> {
    reqwest::blocking::ClientBuilder::new()
        // Following redirects here would open the client up to SSRF.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AuthError::Oauth(e.to_string()))
}

fn receive_redirect(listener: &TcpListener) -> Result<(AuthorizationCode, CsrfToken), AuthError> {
    let (mut stream, _) = listener.accept().map_err(AuthError::Io)?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).map_err(AuthError::Io)?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or(AuthError::MissingRedirectCode)?;
    let url = Url::parse(&format!("http://127.0.0.1{path}"))
        .map_err(|_| AuthError::MissingRedirectCode)?;

    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| AuthorizationCode::new(value.into_owned()))
        .ok_or(AuthError::MissingRedirectCode)?;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| CsrfToken::new(value.into_owned()))
        .ok_or(AuthError::MissingRedirectCode)?;

    let body = "<html><body>Signed in to Calix \u{2014} you can close this tab.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());

    Ok((code, state))
}

fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
