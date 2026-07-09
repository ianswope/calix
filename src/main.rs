use adw::prelude::*;
use gtk::glib;

mod calendar_dialog;
mod config;
mod date_util;
mod event_dialog;
mod google;
mod icloud;
mod store;
mod style;
mod views;
mod window;

const APP_ID: &str = "com.ianswope.Calix";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| style::load());
    app.connect_activate(window::build);
    app.run()
}
