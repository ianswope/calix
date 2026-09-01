use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize, Serialize, Clone)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Where the location field's type-ahead looks things up, when it looks
/// beyond the places this calendar already knows.
#[derive(Deserialize, Serialize, Clone)]
pub struct PlacesConfig {
    /// Whether typed text is sent to a geocoder at all. Off leaves the local
    /// half — locations already used here — working exactly as before.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// The geocoder to ask, for a self-hosted Photon or a compatible service.
    /// Absent or blank means [`crate::places::DEFAULT_ENDPOINT`].
    #[serde(default)]
    pub endpoint: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Deserialize, Serialize, Default)]
pub struct Config {
    pub google: Option<GoogleConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub places: Option<PlacesConfig>,
    /// A user-facing explanation when `config.toml` exists but cannot be
    /// loaded. Kept out of serde so diagnostics never become configuration.
    #[serde(skip)]
    pub load_error: Option<String>,
}

impl Config {
    /// Loads `~/.config/calix/config.toml`. A missing file is the normal
    /// unconfigured state. Existing files that cannot be read or parsed keep
    /// their diagnostic so the graphical account flow can offer a useful fix.
    pub fn load() -> Config {
        Self::load_from(crate::xdg::config_home().join("calix").join("config.toml"))
    }

    fn load_from(path: PathBuf) -> Config {
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Config::default();
            }
            Err(error) => {
                return Self::with_error(format!("Could not read {}: {error}", path.display()));
            }
        };

        match toml::from_str(&contents) {
            Ok(config) => config,
            Err(error) => {
                Self::with_error(format!("Could not understand {}: {error}", path.display()))
            }
        }
    }

    fn with_error(message: String) -> Config {
        eprintln!("calix: {message}");
        Config {
            load_error: Some(message),
            ..Config::default()
        }
    }

    /// The geocoder to ask for location suggestions, or `None` when that half
    /// of the type-ahead is switched off. No `[places]` section means the
    /// default endpoint, which is the unconfigured behavior.
    pub fn places_endpoint(&self) -> Option<&str> {
        let Some(places) = &self.places else {
            return Some(crate::places::DEFAULT_ENDPOINT);
        };
        if !places.enabled {
            return None;
        }
        Some(
            places
                .endpoint
                .as_deref()
                .map(str::trim)
                .filter(|endpoint| !endpoint.is_empty())
                .unwrap_or(crate::places::DEFAULT_ENDPOINT),
        )
    }

    pub fn save_google(client_id: &str, client_secret: &str) -> Result<Config, String> {
        Self::save_google_at(
            &crate::xdg::config_home().join("calix").join("config.toml"),
            client_id,
            client_secret,
        )
    }

    fn save_google_at(
        path: &std::path::Path,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Config, String> {
        let client_id = client_id.trim();
        let client_secret = client_secret.trim();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err("Client ID and client secret are both required.".to_string());
        }
        // Written on top of what is already there. config.toml is hand-edited,
        // and connecting an account must not take away a section the user put
        // in it themselves.
        let config = Config {
            google: Some(GoogleConfig {
                client_id: client_id.to_string(),
                client_secret: client_secret.to_string(),
            }),
            load_error: None,
            ..Self::load_from(path.to_path_buf())
        };
        let directory = path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
        set_owner_only_permissions(&directory, 0o700)
            .map_err(|error| format!("Could not secure {}: {error}", directory.display()))?;
        let contents = toml::to_string_pretty(&config)
            .map_err(|error| format!("Could not encode Google settings: {error}"))?;
        std::fs::write(path, contents)
            .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
        set_owner_only_permissions(path, 0o600)
            .map_err(|error| format!("Could not secure {}: {error}", path.display()))?;
        Ok(config)
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(contents: &str) -> Config {
        toml::from_str(contents).expect("a config file this test wrote")
    }

    #[test]
    fn location_lookup_asks_photon_when_the_file_says_nothing_about_it() {
        assert_eq!(
            parsed("").places_endpoint(),
            Some(crate::places::DEFAULT_ENDPOINT)
        );
    }

    #[test]
    fn location_lookup_can_be_switched_off() {
        // The one setting that stops typed text from leaving the machine. The
        // local half — places this calendar has already used — keeps working.
        let config = parsed("[places]\nenabled = false\n");

        assert_eq!(config.places_endpoint(), None);
    }

    #[test]
    fn a_self_hosted_geocoder_replaces_the_default() {
        let config = parsed("[places]\nendpoint = \"http://localhost:2322/api\"\n");

        assert_eq!(config.places_endpoint(), Some("http://localhost:2322/api"));
    }

    #[test]
    fn a_blank_endpoint_falls_back_rather_than_making_every_lookup_fail() {
        let config = parsed("[places]\nendpoint = \"  \"\n");

        assert_eq!(
            config.places_endpoint(),
            Some(crate::places::DEFAULT_ENDPOINT)
        );
    }

    #[test]
    fn saving_google_credentials_leaves_the_location_settings_alone() {
        // Everything in config.toml is hand-written, and connecting a Google
        // account must not quietly take a section of it away.
        // A directory of its own: saving locks the config's parent down to
        // 0700, and handed the bare temp directory it would try to chmod /tmp.
        let directory = std::env::temp_dir().join(format!(
            "calix-config-places-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        std::fs::write(&path, "[places]\nenabled = false\n").unwrap();

        Config::save_google_at(&path, "an-id", "a-secret").expect("the credentials to save");
        let reloaded = Config::load_from(path.clone());
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(
            reloaded.google.as_ref().map(|g| g.client_id.as_str()),
            Some("an-id")
        );
        assert_eq!(reloaded.places_endpoint(), None);
    }

    #[test]
    fn a_missing_file_is_an_unconfigured_not_broken_state() {
        let config = Config::load_from(PathBuf::from("/definitely/not/a/calix/config.toml"));
        assert!(config.google.is_none());
        assert!(config.load_error.is_none());
    }

    #[test]
    fn malformed_toml_keeps_an_actionable_diagnostic() {
        let path = std::env::temp_dir().join(format!(
            "calix-config-test-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        std::fs::write(&path, "[google\nclient_id = nope").unwrap();
        let config = Config::load_from(path.clone());
        let _ = std::fs::remove_file(path);

        assert!(config.google.is_none());
        assert!(
            config
                .load_error
                .as_deref()
                .unwrap()
                .contains("Could not understand")
        );
    }
}
