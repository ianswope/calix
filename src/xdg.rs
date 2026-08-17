//! XDG base directories, resolved straight from the environment.
//!
//! These used to be `glib::user_*_dir()` calls. Resolving them here instead
//! keeps the storage, config and theme layers free of any GTK dependency, so
//! the whole non-widget half of Calix builds without a display — which is what
//! lets `store.rs` and friends be unit-tested, and leaves the door open to a
//! second frontend that doesn't link GTK at all.
//!
//! Per the XDG base directory spec, a variable holding a relative path is
//! invalid and must be ignored in favour of the default.

use std::path::PathBuf;

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_home() -> PathBuf {
    resolve(env("XDG_DATA_HOME"), env("HOME"), ".local/share")
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> PathBuf {
    resolve(env("XDG_CONFIG_HOME"), env("HOME"), ".config")
}

/// `$XDG_STATE_HOME`, or `~/.local/state`.
pub fn state_home() -> PathBuf {
    resolve(env("XDG_STATE_HOME"), env("HOME"), ".local/state")
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The spec's resolution order: an absolute `var` wins, otherwise the default
/// under `home`. With neither (no `HOME` at all, which shouldn't happen outside
/// a stripped service environment) the bare relative default is returned rather
/// than panicking — a caller writing there fails loudly on its own terms.
fn resolve(var: Option<String>, home: Option<String>, default: &str) -> PathBuf {
    match var.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => match home {
            Some(home) => PathBuf::from(home).join(default),
            None => PathBuf::from(default),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn an_absolute_override_is_used_as_is() {
        assert_eq!(
            resolve(s("/srv/data"), s("/home/ian"), ".local/share"),
            PathBuf::from("/srv/data")
        );
    }

    #[test]
    fn an_unset_override_falls_back_to_the_default_under_home() {
        assert_eq!(
            resolve(None, s("/home/ian"), ".local/share"),
            PathBuf::from("/home/ian/.local/share")
        );
    }

    #[test]
    fn a_relative_override_is_ignored_as_the_spec_requires() {
        assert_eq!(
            resolve(s("relative/data"), s("/home/ian"), ".local/share"),
            PathBuf::from("/home/ian/.local/share")
        );
    }

    #[test]
    fn without_home_or_an_override_the_bare_default_is_returned() {
        assert_eq!(
            resolve(None, None, ".local/state"),
            PathBuf::from(".local/state")
        );
    }
}
