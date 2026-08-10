// Keyring helpers, shared by both CalDAV account types: iCloud (app-specific
// password) and generic CalDAV (server password). Both store a single secret
// under a stable per-account key; only the key derivation differs.
use std::time::Duration;

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
            // Deliberately says the saved password is probably fine. This
            // error means we could not *read* the secret, not that the secret
            // was rejected — and reading "keyring error" as "expired password"
            // is what turns a daemon hiccup into a trip to Apple's website for
            // a replacement password that was never needed.
            CredentialError::Keyring(e) => write!(
                f,
                "couldn't read the saved password from the system keyring: {e}. \
                 The stored password is probably still valid — try \
                 `systemctl --user restart gnome-keyring-daemon` and sync again \
                 before creating a new one."
            ),
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

/// Removes the saved secret for `token_key` from this machine's keyring.
/// Already-absent counts as success, so disconnecting an account whose
/// credential was lost still succeeds.
///
/// Local only: this does not revoke anything with the provider. An
/// app-specific password stays valid at Apple until revoked there, which is
/// deliberate — a stale one costs nothing, and revoking is irreversible.
pub fn delete_password(token_key: &str) -> Result<(), CredentialError> {
    match keyring_entry(token_key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredentialError::Keyring(e)),
    }
}

/// How many times to attempt a keyring read before giving up.
const READ_ATTEMPTS: u32 = 3;

/// Pause between read attempts — long enough to let the daemon settle, short
/// enough to stay invisible behind a sync that is already doing network I/O.
const RETRY_DELAY: Duration = Duration::from_millis(150);

/// Whether a keyring failure is worth a second attempt.
///
/// `PlatformFailure` and `NoStorageAccess` both wrap the underlying D-Bus
/// error, so between them they cover the flake this retry exists for: the
/// daemon drops the object path an entry resolved to, and `zbus` reports
/// `UnknownMethod` for a secret that is still perfectly well stored. A locked
/// collection lands here too, and unlocks moments later. `NoDefaultStore` is
/// the store-registration race `prime_keyring_store` guards against, which
/// resolves as soon as the registering thread finishes.
///
/// Everything else — above all `NoEntry` — is a definite answer, and retrying
/// it would only delay an honest error.
fn is_transient(error: &keyring::Error) -> bool {
    matches!(
        error,
        keyring::Error::PlatformFailure(_)
            | keyring::Error::NoStorageAccess(_)
            | keyring::Error::NoDefaultStore
    )
}

/// Runs `read`, retrying while it fails for a transient reason.
///
/// Generic over the read so tests can drive every branch without a live
/// keyring or a D-Bus session.
fn retry_transient<T>(
    attempts: u32,
    delay: Duration,
    mut read: impl FnMut() -> Result<T, keyring::Error>,
) -> Result<T, keyring::Error> {
    let mut remaining = attempts.max(1);
    loop {
        remaining -= 1;
        match read() {
            Err(e) if remaining > 0 && is_transient(&e) => {
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
            }
            result => return result,
        }
    }
}

pub fn app_password(token_key: &str) -> Result<Option<String>, CredentialError> {
    // Build the entry inside the retry rather than outside it: a stale D-Bus
    // object path is cached on the entry, so reusing one would retry against
    // the same dead path. Constructing it again re-resolves the secret.
    let read = || keyring::Entry::new(KEYRING_SERVICE, token_key)?.get_password();
    match retry_transient(READ_ATTEMPTS, RETRY_DELAY, read) {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredentialError::Keyring(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A read that fails `failures` times with `error()`, then yields "secret".
    fn flaky(
        failures: usize,
        error: fn() -> keyring::Error,
    ) -> impl FnMut() -> Result<String, keyring::Error> {
        let remaining = Cell::new(failures);
        move || match remaining.get() {
            0 => Ok("secret".to_string()),
            n => {
                remaining.set(n - 1);
                Err(error())
            }
        }
    }

    fn dbus_flake() -> keyring::Error {
        keyring::Error::PlatformFailure("UnknownMethod: no such object path".into())
    }

    #[test]
    fn a_dropped_dbus_object_path_is_retried_until_the_read_succeeds() {
        let read = flaky(1, dbus_flake);
        let password = retry_transient(READ_ATTEMPTS, Duration::ZERO, read);
        assert_eq!(password.unwrap(), "secret");
    }

    #[test]
    fn a_locked_collection_is_retried() {
        let read = flaky(2, || {
            keyring::Error::NoStorageAccess("collection is locked".into())
        });
        let password = retry_transient(READ_ATTEMPTS, Duration::ZERO, read);
        assert_eq!(password.unwrap(), "secret");
    }

    #[test]
    fn a_missing_entry_is_answered_immediately_rather_than_retried() {
        let attempts = Cell::new(0);
        let read = || -> Result<String, keyring::Error> {
            attempts.set(attempts.get() + 1);
            Err(keyring::Error::NoEntry)
        };
        let result = retry_transient(READ_ATTEMPTS, Duration::ZERO, read);
        assert!(matches!(result, Err(keyring::Error::NoEntry)));
        assert_eq!(attempts.get(), 1, "a definite answer must not be retried");
    }

    #[test]
    fn a_read_that_never_recovers_gives_up_after_the_attempt_limit() {
        let attempts = Cell::new(0);
        let read = || -> Result<String, keyring::Error> {
            attempts.set(attempts.get() + 1);
            Err(dbus_flake())
        };
        let result = retry_transient(READ_ATTEMPTS, Duration::ZERO, read);
        assert!(result.is_err());
        assert_eq!(attempts.get(), READ_ATTEMPTS as usize);
    }

    #[test]
    fn a_keyring_read_failure_says_the_saved_password_is_probably_still_good() {
        let message = CredentialError::Keyring(dbus_flake()).to_string();
        assert!(
            message.contains("still valid"),
            "a keyring read failure must not read as a dead password: {message}"
        );
        assert!(
            message.contains("gnome-keyring"),
            "the message should name the thing to restart: {message}"
        );
    }
}
