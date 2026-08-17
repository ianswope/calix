use adw::prelude::*;
use gtk::glib;

mod build_info;
mod caldav;
mod calendar_dialog;
mod config;
mod date_util;
mod event_dialog;
mod event_popover;
mod google;
mod http;
mod icloud;
mod notify;
mod omarchy;
mod recurrence;
mod search;
mod store;
mod style;
mod sync;
mod views;
mod window;
mod xdg;

const APP_ID: &str = "com.ianswope.Calix";

fn main() -> glib::ExitCode {
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
