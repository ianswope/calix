//! XDG autostart integration for keeping sync and event alerts alive.

use std::path::PathBuf;

const FILE_NAME: &str = "com.ianswope.Calix.desktop";

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
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Calix Background Alerts\nExec={} --gapplication-service\nIcon=com.ianswope.Calix\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        executable.display()
    );
    std::fs::write(path, entry).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_file_has_the_standard_name() {
        assert_eq!(
            path().file_name().and_then(|name| name.to_str()),
            Some(FILE_NAME)
        );
    }
}
