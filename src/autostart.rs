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
    enabled_from(sandboxed(), path().is_file(), read_recorded())
}

/// A Flatpak sandbox is recognizable by this file. The host session does not
/// scan the autostart directory inside the sandbox, so the file Calix used to
/// write there never launched anything.
pub fn sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").is_file()
}

fn uses_portal(sandboxed: bool) -> bool {
    sandboxed
}

/// What `enabled()` reports. A sandbox must not treat its own autostart file as
/// proof: that file is invisible to the login session. The portal has no way
/// to ask again later, so the last answer it gave is what the switch shows.
fn enabled_from(sandboxed: bool, file_exists: bool, recorded: Option<bool>) -> bool {
    if sandboxed {
        recorded.unwrap_or(false)
    } else {
        file_exists
    }
}

/// The command the portal rewrites into `flatpak run --command=calix <app> …`.
fn portal_command() -> [&'static str; 2] {
    ["calix", BACKGROUND_FLAG]
}

/// `response` is the portal's Response code: 0 granted, 1 dismissed, 2 failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PortalReply {
    response: u32,
    /// `None` when the reply did not include the autostart bit.
    autostart: Option<bool>,
}

/// The autostart bit to store. A failed request does not carry a new answer,
/// and neither does a dismissal that left the bit out.
fn recorded_autostart(reply: PortalReply) -> Option<bool> {
    if reply.response > 1 {
        return None;
    }
    reply.autostart
}

fn portal_failure(requested: bool, reply: PortalReply) -> Option<&'static str> {
    match recorded_autostart(reply) {
        Some(enabled) if enabled == requested => None,
        Some(_) if requested => Some("Calix was not allowed to start when you sign in"),
        Some(_) => Some("Calix is still set to start when you sign in"),
        None => Some("the system could not change background startup"),
    }
}

fn parse_recorded(text: &str) -> bool {
    text.trim() == "true"
}

fn recorded_path() -> PathBuf {
    crate::xdg::config_home()
        .join("calix")
        .join("flatpak-autostart")
}

fn read_recorded() -> Option<bool> {
    let text = std::fs::read_to_string(recorded_path()).ok()?;
    Some(parse_recorded(&text))
}

fn write_recorded(enabled: bool) -> Result<(), String> {
    let path = recorded_path();
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid autostart state path".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let text = if enabled { "true\n" } else { "false\n" };
    std::fs::write(path, text).map_err(|error| error.to_string())
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if uses_portal(sandboxed()) {
        return set_enabled_via_portal(enabled);
    }
    set_enabled_via_file(enabled)
}

fn set_enabled_via_file(enabled: bool) -> Result<(), String> {
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

/// Asks the Background portal and stores the autostart bit it answers with.
///
/// The portal writes the login entry on the host, outside the sandbox. It does
/// not offer a way to read that entry back, so the bit in its Response is the
/// answer `enabled()` keeps.
fn set_enabled_via_portal(enabled: bool) -> Result<(), String> {
    let reply = request_background(enabled)?;
    if let Some(recorded) = recorded_autostart(reply) {
        write_recorded(recorded)?;
        // An older build left a desktop file in the sandbox. The host never
        // read it, and it is not the switch's source any more.
        if path().exists() {
            std::fs::remove_file(path()).map_err(|error| error.to_string())?;
        }
    }
    match portal_failure(enabled, reply) {
        Some(message) => Err(message.to_string()),
        None => Ok(()),
    }
}

const PORTAL_REASON: &str = "Keep calendar sync and event alerts working after you sign in";

fn background_options<'a>(
    enabled: bool,
    token: &'a str,
) -> std::collections::HashMap<&'static str, zbus::zvariant::Value<'a>> {
    use zbus::zvariant::Value;

    let mut options = std::collections::HashMap::new();
    options.insert("handle_token", Value::from(token));
    options.insert("autostart", Value::from(enabled));
    options.insert("reason", Value::from(PORTAL_REASON));
    // `as`, a list of strings. A list of variants is a different type and the
    // portal ignores it, which leaves the host entry without `--background`.
    options.insert("commandline", Value::from(portal_command().to_vec()));
    options
}

fn request_background(enabled: bool) -> Result<PortalReply, String> {
    let connection = zbus::blocking::Connection::session().map_err(|error| error.to_string())?;
    let sender = connection
        .unique_name()
        .map(|name| name.trim_start_matches(':').replace('.', "_"))
        .ok_or_else(|| "the session bus did not name this connection".to_string())?;
    let token = format!(
        "calix_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

    let request = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|error| error.to_string())?;
    // Polled on another thread for the whole call. A reply that arrives before
    // RequestBackground returns has to be taken off the bus; an unpolled stream
    // can stall the connection.
    let mut signals = request
        .receive_signal("Response")
        .map_err(|error| error.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(signals.next());
    });

    let reply = receive_portal_reply(&connection, &request_path, &token, enabled, &rx);
    if reply.is_err() {
        // Close unblocks the waiter when the portal never answers. A request
        // that already finished ignores it.
        let _: Result<(), _> = request.call("Close", &());
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    }
    reply
}

fn receive_portal_reply(
    connection: &zbus::blocking::Connection,
    request_path: &str,
    token: &str,
    enabled: bool,
    rx: &std::sync::mpsc::Receiver<Option<zbus::Message>>,
) -> Result<PortalReply, String> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    let options = background_options(enabled, token);
    let background = zbus::blocking::Proxy::new(
        connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Background",
    )
    .map_err(|error| error.to_string())?;
    let returned: OwnedObjectPath = background
        .call("RequestBackground", &("", &options))
        .map_err(|error| format!("couldn't reach the background portal: {error}"))?;
    if returned.as_str() != request_path {
        if let Ok(other) = zbus::blocking::Proxy::new(
            connection,
            "org.freedesktop.portal.Desktop",
            returned.as_str(),
            "org.freedesktop.portal.Request",
        ) {
            let _: Result<(), _> = other.call("Close", &());
        }
        return Err("the background portal answered on an unexpected path".to_string());
    }

    let message = rx
        .recv_timeout(std::time::Duration::from_secs(120))
        .map_err(|_| "the background request was not answered".to_string())?
        .ok_or_else(|| "the background request ended without an answer".to_string())?;
    let (response, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    let autostart = results
        .get("autostart")
        .and_then(|value| bool::try_from(value).ok());
    Ok(PortalReply {
        response,
        autostart,
    })
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

    #[test]
    fn a_sandbox_ignores_its_autostart_file_and_uses_the_portals_answer() {
        assert!(
            uses_portal(true),
            "a Flatpak install has to ask the background portal"
        );
        assert!(!uses_portal(false));
        assert!(
            !enabled_from(true, true, Some(false)),
            "a file inside the sandbox is not a login entry"
        );
        assert!(enabled_from(true, false, Some(true)));
        assert!(!enabled_from(true, true, None));
        assert!(enabled_from(false, true, None));
        assert!(
            !enabled_from(false, false, Some(true)),
            "outside the sandbox the desktop file is the entry"
        );
    }

    #[test]
    fn the_portal_launches_calix_in_the_background() {
        let command = portal_command();
        let args: Vec<String> = command.iter().map(|arg| (*arg).to_string()).collect();
        assert!(
            is_background_launch(&args),
            "login must start the alert process, not a window: {args:?}"
        );
    }

    #[test]
    fn the_switch_follows_the_portals_autostart_answer() {
        let granted = PortalReply {
            response: 0,
            autostart: Some(true),
        };
        assert_eq!(recorded_autostart(granted), Some(true));
        assert_eq!(portal_failure(true, granted), None);

        let refused = PortalReply {
            response: 1,
            autostart: Some(false),
        };
        assert_eq!(recorded_autostart(refused), Some(false));
        assert!(portal_failure(true, refused).is_some());

        let dismissed = PortalReply {
            response: 1,
            autostart: None,
        };
        assert_eq!(
            recorded_autostart(dismissed),
            None,
            "a dismissal that left the bit out is not a new answer"
        );
        assert!(portal_failure(false, dismissed).is_some());

        let broken = PortalReply {
            response: 2,
            autostart: Some(true),
        };
        assert_eq!(
            recorded_autostart(broken),
            None,
            "a failed request is not a new answer"
        );
        assert!(portal_failure(true, broken).is_some());
    }

    #[test]
    fn the_portal_command_is_a_list_of_strings() {
        let options = background_options(true, "calix_1_2");
        let command = options.get("commandline").expect("commandline option");
        assert_eq!(
            command.value_signature().to_string(),
            "as",
            "the portal reads commandline as an array of strings, not variants"
        );
    }

    #[test]
    fn a_recorded_portal_answer_is_only_the_word_true() {
        assert!(parse_recorded("true\n"));
        assert!(!parse_recorded("false\n"));
        assert!(!parse_recorded(""));
    }
}
