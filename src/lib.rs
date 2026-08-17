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

pub mod build_info;
pub mod caldav;
pub mod config;
pub mod date_util;
pub mod google;
pub mod http;
pub mod icloud;
pub mod notify;
pub mod omarchy;
pub mod provider;
pub mod recurrence;
pub mod store;
pub mod sync;
pub mod xdg;

#[cfg(feature = "gui")]
mod calendar_dialog;
#[cfg(feature = "gui")]
mod event_dialog;
#[cfg(feature = "gui")]
mod event_popover;
#[cfg(feature = "gui")]
mod search;
#[cfg(feature = "gui")]
mod style;
#[cfg(feature = "gui")]
mod views;
#[cfg(feature = "gui")]
mod window;

/// Runs the GTK application, argv handling and all.
#[cfg(feature = "gui")]
pub fn run() -> gtk::glib::ExitCode {
    use adw::prelude::*;
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

    // Same line into the journal, so a bug report from a running instance
    // carries its provenance without anyone having to think of it.
    eprintln!("calix {}", build_info::stamp());

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| style::load());
    app.connect_activate(window::build);
    app.run()
}
