// Keyring helpers, shared by both CalDAV account types: iCloud (app-specific
// password) and generic CalDAV (server password). Both store a single secret
// under a stable per-account key; only the key derivation differs.
const KEYRING_SERVICE: &str = "com.ianswope.Calix";
const KEYRING_USERNAME_PREFIX: &str = "icloud-app-password";
const CALDAV_KEYRING_PREFIX: &str = "caldav-password";

#[derive(Debug)]
pub enum CredentialError {
    Keyring(keyring::Error),
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialError::Keyring(e) => write!(f, "couldn't access the system keyring: {e}"),
        }
    }
}

fn keyring_entry(token_key: &str) -> Result<keyring::Entry, CredentialError> {
    keyring::Entry::new(KEYRING_SERVICE, token_key).map_err(CredentialError::Keyring)
}

pub fn token_key(apple_id: &str) -> String {
    format!(
        "{KEYRING_USERNAME_PREFIX}:{}",
        apple_id.trim().to_lowercase()
    )
}

/// Keyring key for a generic CalDAV account. Includes the server so the same
/// username on two different servers gets distinct secrets. Callers must pass
/// a URL from `caldav::canonical_base_url` — the same canonical form used for
/// the account row — so the keyring and the database agree on identity.
pub fn caldav_token_key(canonical_base_url: &str, username: &str) -> String {
    format!(
        "{CALDAV_KEYRING_PREFIX}:{canonical_base_url}|{}",
        username.trim().to_lowercase()
    )
}

pub fn save_app_password(token_key: &str, app_password: &str) -> Result<(), CredentialError> {
    keyring_entry(token_key)?
        .set_password(app_password)
        .map_err(CredentialError::Keyring)
}

pub fn app_password(token_key: &str) -> Result<Option<String>, CredentialError> {
    match keyring_entry(token_key)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredentialError::Keyring(e)),
    }
}
