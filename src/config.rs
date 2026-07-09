use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize, Default)]
pub struct Config {
    pub google: Option<GoogleConfig>,
}

impl Config {
    /// Loads `~/.config/calix/config.toml`. Missing file or a Google
    /// section that isn't there yet both just mean "Google isn't
    /// configured" — not an error, since that's the normal state until a
    /// user follows the README's OAuth client setup steps.
    pub fn load() -> Config {
        let path = gtk::glib::user_config_dir()
            .join("calix")
            .join("config.toml");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Config::default();
        };
        toml::from_str(&contents).unwrap_or_else(|e| {
            eprintln!("calix: failed to parse {}: {e}", path.display());
            Config::default()
        })
    }
}
