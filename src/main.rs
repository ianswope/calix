//! The GTK binary. Everything it does lives in the library, so the backend can
//! be linked without a display — see `lib.rs`.

fn main() -> gtk::glib::ExitCode {
    calix::run()
}
