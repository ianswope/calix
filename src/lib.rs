//! Calix, split into a backend that knows about calendars and a frontend that
//! knows about widgets.
//!
//! Everything above the `gui` feature gate compiles without GTK: storage,
//! recurrence expansion, CalDAV and Google sync, alert scheduling, date math.
//! That isn't incidental tidiness — it's what lets the bulk of the suite run
//! headless, and it's the seam a second frontend would attach to. `cargo check
//! --no-default-features` is the thing that keeps the claim honest; if a `use
//! gtk::` ever drifts down here, that build is what fails.
//!
//! The GTK frontend lives behind `gui` (on by default) and is what the `calix`
//! binary runs.

pub mod agenda;
pub mod autostart;
pub mod build_info;
pub mod caldav;
pub mod config;
pub mod date_util;
pub mod google;
pub mod http;
pub mod icloud;
pub mod notify;
pub mod omarchy;
pub mod places;
pub mod provider;
pub mod recurrence;
pub mod store;
pub mod sync;
pub mod undo;
pub mod xdg;

#[cfg(feature = "gui")]
mod calendar_dialog;
#[cfg(feature = "gui")]
mod event_dialog;
#[cfg(feature = "gui")]
mod event_popover;
#[cfg(feature = "gui")]
mod location_completion;
#[cfg(feature = "gui")]
mod search;
#[cfg(feature = "gui")]
mod style;
#[cfg(feature = "gui")]
mod views;
#[cfg(feature = "gui")]
mod window;

#[cfg(all(test, feature = "gui"))]
mod gui_leaks;

/// Runs the GTK application, argv handling and all.
#[cfg(feature = "gui")]
pub fn run() -> gtk::glib::ExitCode {
    use adw::prelude::*;
    use gtk::gio;
    use gtk::glib;

    const APP_ID: &str = "com.ianswope.Calix";

    // Handled before GTK sees argv: the installed binary is a copy, so knowing
    // which commit it came from is the only way to tell a stale install from a
    // real bug. `scripts/check-installed.sh` reads this.
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("calix {}", build_info::stamp());
        return glib::ExitCode::SUCCESS;
    }

    // Whether the desktop theme was picked up is invisible from a screenshot
    // once it's wrong — a missing palette and a palette that parsed to stock
    // colors look identical. This prints what was actually resolved, without
    // needing a display.
    if std::env::args().any(|arg| arg == "--print-theme") {
        match omarchy::theme_overrides() {
            Some(overrides) => {
                println!("# resolved Omarchy theme (dark = {})", overrides.dark);
                print!("{}", overrides.css);
            }
            None => println!("# no Omarchy theme found; using stock Adwaita colors"),
        }
        return glib::ExitCode::SUCCESS;
    }

    // Both agenda command lines are answered here, before GTK: they exist for
    // a status-bar widget to read on a timer, and reading the day's meetings
    // must never be the thing that opens a window or holds the app alive.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = agenda::handle_cli(&args) {
        return match code {
            0 => glib::ExitCode::SUCCESS,
            _ => glib::ExitCode::FAILURE,
        };
    }

    // Same line into the journal, so a bug report from a running instance
    // carries its provenance without anyone having to think of it.
    eprintln!("calix {}", build_info::stamp());

    // Checked here, in the process the user typed into, so a bad date prints
    // where they can see it. Once forwarded, stderr belongs to whichever
    // instance started first and the complaint would land in its journal.
    if let Err(message) = date_util::parse_date_arg(&args) {
        eprintln!("calix: {message}");
        return glib::ExitCode::FAILURE;
    }

    // HANDLES_COMMAND_LINE so `calix 2026-08-22` reaches the instance that is
    // already running: GApplication forwards argv to it, and the date moves
    // the window that's up rather than opening a second one beside it.
    let non_unique = std::env::var_os("CALIX_NON_UNIQUE").is_some();
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    // A clean-profile UX or integration test must be able to coexist with a
    // normally running Calix while still using the real desktop/keyring bus.
    // This opt-in is intentionally environment-only and never affects an
    // ordinary launch from the application menu.
    if non_unique {
        flags = gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();
    // Calix doubles as the lightweight alert process, but only when it was
    // asked to: a login launch, or the autostart option being on, keeps sync
    // and reminder timers alive after the window closes. An ordinary launch
    // still exits with its window, and `app.quit` ends either one.
    let _background_hold =
        autostart::keeps_running_without_a_window(&args, autostart::enabled()).then(|| app.hold());
    app.connect_startup(|app| {
        style::load();
        let quit = gio::SimpleAction::new("quit", None);
        let app_for_quit = app.clone();
        quit.connect_activate(move |_, _| app_for_quit.quit());
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
    });
    if non_unique {
        app.connect_activate(|app| window::open(app, None));
    }
    app.connect_command_line(|app, command_line| {
        let args: Vec<String> = command_line
            .arguments()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let background = autostart::is_background_launch(&args);
        // The invoking process rejects a bad date before it forwards, so one
        // can't arrive here; opening on today beats refusing to open at all.
        if background && app.active_window().is_none() {
            window::start_background(app);
        } else {
            window::open(app, date_util::parse_date_arg(&args).unwrap_or(None));
        }
        glib::ExitCode::SUCCESS
    });
    app.run()
}
