//! XDG autostart integration for keeping sync and event alerts alive.

use std::path::PathBuf;

const FILE_NAME: &str = "com.ianswope.Calix.desktop";

/// The switch the autostart entry passes so a login launch stays windowless.
///
/// GLib's own `--gapplication-service` cannot be used for this: for an
/// application that is not explicitly flagged as a service, the default
/// `local_command_line` handler intercepts that switch and *interrupts* normal
/// command-line processing, so the `command-line` handler — and with it the
/// decision to start without a window — never runs at all.
pub const BACKGROUND_FLAG: &str = "--background";

/// Whether `args` (as passed to the process, argv\[0\] included) asks Calix to
/// start as the background alert process rather than open a window.
pub fn is_background_launch(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == BACKGROUND_FLAG)
}

/// Whether Calix should outlive its window. The alert process is meant to keep
/// running only when the user asked for it — a login launch, or the autostart
/// option left on — so an ordinary launch still exits with its window.
pub fn keeps_running_without_a_window(args: &[String], autostart_enabled: bool) -> bool {
    is_background_launch(args) || autostart_enabled
}

pub fn path() -> PathBuf {
    crate::xdg::config_home().join("autostart").join(FILE_NAME)
}

pub fn enabled() -> bool {
    path().is_file()
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let path = path();
    if !enabled {
        if path.exists() {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid autostart path".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    std::fs::write(path, entry(&executable)).map_err(|error| error.to_string())
}

/// The autostart desktop entry that launches `executable` in background mode.
fn entry(executable: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Calix Background Alerts\nExec={executable} {BACKGROUND_FLAG}\nIcon=com.ianswope.Calix\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        executable = executable.display()
    )
}

/// The `Exec=` arguments of `entry`, as the launched process would see them.
#[cfg(test)]
fn entry_launch_args(entry: &str) -> Vec<String> {
    entry
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .expect("the entry has an Exec line")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_autostart_entry_asks_for_a_background_launch() {
        let args = entry_launch_args(&entry(std::path::Path::new("/usr/bin/calix")));
        assert!(
            is_background_launch(&args),
            "the entry Calix writes must be recognized by the flag it parses: {args:?}"
        );
    }

    #[test]
    fn an_ordinary_launch_is_not_a_background_launch() {
        let args = vec!["calix".to_string(), "2026-08-21".to_string()];
        assert!(!is_background_launch(&args));
    }

    #[test]
    fn glibs_service_switch_no_longer_stands_in_for_background_mode() {
        // GLib eats this one before the application ever sees it, so an entry
        // carrying it would start a process that does nothing.
        let args = vec!["calix".to_string(), "--gapplication-service".to_string()];
        assert!(!is_background_launch(&args));
    }

    #[test]
    fn an_ordinary_launch_with_the_option_off_exits_with_its_window() {
        let args = vec!["calix".to_string()];
        assert!(!keeps_running_without_a_window(&args, false));
    }

    #[test]
    fn a_login_launch_keeps_running_without_a_window() {
        let args = entry_launch_args(&entry(std::path::Path::new("/usr/bin/calix")));
        assert!(keeps_running_without_a_window(&args, false));
    }

    #[test]
    fn with_the_option_on_an_ordinary_launch_also_keeps_running() {
        let args = vec!["calix".to_string()];
        assert!(keeps_running_without_a_window(&args, true));
    }

    #[test]
    fn autostart_file_has_the_standard_name() {
        assert_eq!(
            path().file_name().and_then(|name| name.to_str()),
            Some(FILE_NAME)
        );
    }
}
