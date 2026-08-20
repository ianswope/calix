use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize, Serialize, Clone)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize, Serialize, Default)]
pub struct Config {
    pub google: Option<GoogleConfig>,
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

    pub fn save_google(client_id: &str, client_secret: &str) -> Result<Config, String> {
        let client_id = client_id.trim();
        let client_secret = client_secret.trim();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err("Client ID and client secret are both required.".to_string());
        }
        let config = Config {
            google: Some(GoogleConfig {
                client_id: client_id.to_string(),
                client_secret: client_secret.to_string(),
            }),
            load_error: None,
        };
        let directory = crate::xdg::config_home().join("calix");
        let path = directory.join("config.toml");
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
        set_owner_only_permissions(&directory, 0o700)
            .map_err(|error| format!("Could not secure {}: {error}", directory.display()))?;
        let contents = toml::to_string_pretty(&config)
            .map_err(|error| format!("Could not encode Google settings: {error}"))?;
        std::fs::write(&path, contents)
            .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
        set_owner_only_permissions(&path, 0o600)
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
