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

/// Register the process-wide keyring store from the calling thread.
///
/// `keyring` 4.1.4's `v1` layer sets up the global credential store lazily on
/// the first `Entry::new`, but it flips its "initialized" flag *before* the
/// store is actually registered. Our launch/resume sync spawns the Google,
/// iCloud, and CalDAV workers at once, so a thread that loses that race sees the
/// flag already set, skips initialization, and fails with `NoDefaultStore`
/// ("no default store has been set, so cannot search or create entries").
///
/// Calling this once on the main thread at startup — before any sync thread
/// spawns — wins the race deterministically, so every later `Entry::new` on any
/// thread finds the store ready. The store is global, so this also covers
/// Google's entries, not just CalDAV's.
pub fn prime_keyring_store() {
    // Any `Entry::new` triggers the one-time store registration; the username
    // need not exist, since we never read it — only the init side effect matters.
    if let Err(e) = keyring::Entry::new(KEYRING_SERVICE, "store-warmup") {
        eprintln!("calix: keyring store did not initialize at startup: {e}");
    }
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
