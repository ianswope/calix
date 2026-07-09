use crate::config::Config;
use crate::date_util::{
    month_grid_bounds, month_start, shift_months, shift_weeks, week_bounds, week_dates, week_start,
};
use crate::event_dialog;
use crate::google;
use crate::store::{self, Event, Store};
use crate::views::{month_view, week_view};
use adw::prelude::*;
use chrono::{DateTime, Local, NaiveDate};
use gtk::glib;
use gtk::glib::clone;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Month,
    Week,
}

struct State {
    view_mode: ViewMode,
    current_date: NaiveDate,
}

impl State {
    fn period_anchor(&self) -> NaiveDate {
        match self.view_mode {
            ViewMode::Month => month_start(self.current_date),
            ViewMode::Week => week_start(self.current_date),
        }
    }

    fn shift(&self, delta: i32) -> NaiveDate {
        self.shift_from(self.current_date, delta)
    }

    fn shift_from(&self, date: NaiveDate, delta: i32) -> NaiveDate {
        match self.view_mode {
            ViewMode::Month => shift_months(date, delta),
            ViewMode::Week => shift_weeks(date, delta),
        }
    }

    fn title(&self) -> String {
        match self.view_mode {
            ViewMode::Month => self.period_anchor().format("%B %Y").to_string(),
            ViewMode::Week => {
                let days = week_dates(self.current_date);
                let (start, end) = (days[0], days[6]);
                if start.format("%b").to_string() == end.format("%b").to_string() {
                    format!("{} – {}", start.format("%b %-d"), end.format("%-d, %Y"))
                } else {
                    format!("{} – {}", start.format("%b %-d"), end.format("%b %-d, %Y"))
                }
            }
        }
    }
}

/// Bundles the widgets `reset` and the interactive handlers both need, so
/// they don't have to be threaded through as a long parameter list.
struct Ui {
    carousel: adw::Carousel,
    title_label: gtk::Label,
    toast_overlay: adw::ToastOverlay,
    state: Rc<RefCell<State>>,
    store: Rc<Store>,
    config: Rc<Config>,
    rebuilding: Rc<Cell<bool>>,
}

impl Ui {
    /// Clears the carousel and rebuilds it with prev/current/next pages
    /// centered on the current period. Deliberately avoids `scroll_to`:
    /// in this libadwaita version it's unreliable when the target was
    /// just appended in the same call (confirmed by trial — it works
    /// maybe half the time, silently leaving the carousel parked on the
    /// wrong page the rest). `append` on an empty carousel making position
    /// 0 correct by construction, then `prepend`ing `prev`, sidesteps the
    /// bug entirely: no jump is ever requested, only structural inserts.
    fn reset(self: &Rc<Self>) {
        self.rebuilding.set(true);

        let mut child = self.carousel.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.carousel.remove(&widget);
            child = next;
        }

        let state = self.state.borrow();
        let view_mode = state.view_mode;
        let anchor = state.period_anchor();
        let prev_date = state.shift_from(anchor, -1);
        let next_date = state.shift_from(anchor, 1);
        let title = state.title();
        drop(state);

        let current_page = self.build_page(view_mode, anchor);
        let prev_page = self.build_page(view_mode, prev_date);
        let next_page = self.build_page(view_mode, next_date);

        self.carousel.append(&current_page);
        self.carousel.prepend(&prev_page);
        self.carousel.append(&next_page);

        self.rebuilding.set(false);
        self.title_label.set_label(&title);
    }

    /// Builds one page (month grid or week grid) for `date`, wired up to
    /// query this page's events from the store and to open the event
    /// dialog on create/edit clicks.
    fn build_page(self: &Rc<Self>, view_mode: ViewMode, date: NaiveDate) -> gtk::Widget {
        let on_create: Rc<dyn Fn(DateTime<Local>)> = {
            let ui = self.clone();
            Rc::new(move |start: DateTime<Local>| {
                let calendar_id = ui.store.default_calendar_id();
                let ui_for_saved = ui.clone();
                event_dialog::open(&ui.carousel, ui.store.clone(), calendar_id, None, start, move || {
                    ui_for_saved.reset()
                });
            })
        };
        let on_edit: Rc<dyn Fn(Event)> = {
            let ui = self.clone();
            Rc::new(move |event: Event| {
                if event.google_event_id.is_some() {
                    // Editing would just get silently overwritten by the
                    // next sync, so don't pretend it's supported.
                    ui.toast_overlay.add_toast(adw::Toast::new(
                        "Editing synced Google events isn't supported yet",
                    ));
                    return;
                }
                let calendar_id = event.calendar_id;
                let start = event.start;
                let ui_for_saved = ui.clone();
                event_dialog::open(
                    &ui.carousel,
                    ui.store.clone(),
                    calendar_id,
                    Some(event),
                    start,
                    move || ui_for_saved.reset(),
                );
            })
        };

        match view_mode {
            ViewMode::Month => {
                let (range_start, range_end) = month_grid_bounds(date);
                let events = self
                    .store
                    .events_between(store::day_start(range_start), store::day_start(range_end))
                    .unwrap_or_default();
                month_view::build(date, &events, on_create, on_edit)
            }
            ViewMode::Week => {
                let (range_start, range_end) = week_bounds(date);
                let events = self
                    .store
                    .events_between(store::day_start(range_start), store::day_start(range_end))
                    .unwrap_or_default();
                week_view::build(date, &events, on_create, on_edit)
            }
        }
    }
}

pub fn build(app: &adw::Application) {
    let state = Rc::new(RefCell::new(State {
        view_mode: ViewMode::Month,
        current_date: Local::now().date_naive(),
    }));

    let carousel = adw::Carousel::builder()
        .allow_scroll_wheel(true)
        .hexpand(true)
        .vexpand(true)
        .build();

    let store = Rc::new(Store::open().expect("failed to open Calix's local database"));

    let ui = Rc::new(Ui {
        carousel: carousel.clone(),
        title_label: gtk::Label::builder().css_classes(["title"]).build(),
        toast_overlay: adw::ToastOverlay::new(),
        state,
        store,
        config: Rc::new(Config::load()),
        // Guards against `page-changed`/`toggled` firing (and reentering
        // `rebuild`) as a side effect of our own programmatic changes.
        rebuilding: Rc::new(Cell::new(false)),
    });

    let today_button = gtk::Button::builder().label("Today").build();
    let prev_button = gtk::Button::from_icon_name("go-previous-symbolic");
    let next_button = gtk::Button::from_icon_name("go-next-symbolic");
    let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_box.add_css_class("linked");
    nav_box.append(&prev_button);
    nav_box.append(&next_button);

    let month_toggle = gtk::ToggleButton::builder()
        .label("Month")
        .active(true)
        .build();
    let week_toggle = gtk::ToggleButton::builder()
        .label("Week")
        .group(&month_toggle)
        .build();
    let view_toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    view_toggle_box.add_css_class("linked");
    view_toggle_box.append(&month_toggle);
    view_toggle_box.append(&week_toggle);

    let new_event_button = gtk::Button::from_icon_name("list-add-symbolic");
    new_event_button.set_tooltip_text(Some("New Event"));
    new_event_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let calendar_id = ui.store.default_calendar_id();
            let start = next_half_hour();
            let ui2 = ui.clone();
            event_dialog::open(&ui.carousel, ui.store.clone(), calendar_id, None, start, move || {
                ui2.reset()
            });
        }
    ));

    let google_button = gtk::Button::new();
    set_google_button_label(&google_button);
    google_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        google_button,
        move |_| connect_or_sync_google(&ui, &google_button)
    ));

    let header = adw::HeaderBar::new();
    header.pack_start(&today_button);
    header.pack_start(&nav_box);
    header.set_title_widget(Some(&ui.title_label));
    header.pack_end(&view_toggle_box);
    header.pack_end(&new_event_button);
    header.pack_end(&google_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&carousel));

    ui.toast_overlay.set_child(Some(&toolbar_view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Calix")
        .default_width(1100)
        .default_height(750)
        .content(&ui.toast_overlay)
        .build();

    window.present();

    // Defer everything interactive until the carousel has actually been
    // allocated real geometry. Two problems otherwise: (1) scroll_to()
    // computes its jump as a pixel offset (position * width); called while
    // width is still 0 it silently resolves to an offset of 0 for any
    // target and never leaves the first page. (2) GTK's own startup
    // machinery (toggle-group resolution, the carousel's initial position
    // notify) fires a flurry of signals while the window first realizes;
    // connecting our handlers only after that settles keeps them from
    // being mistaken for real user input.
    carousel.add_tick_callback(clone!(
        #[strong]
        ui,
        #[strong]
        today_button,
        #[strong]
        prev_button,
        #[strong]
        next_button,
        #[strong]
        month_toggle,
        #[strong]
        week_toggle,
        move |carousel, _clock| {
            if carousel.width() <= 0 {
                return glib::ControlFlow::Continue;
            }

            ui.reset();
            connect_handlers(
                &ui,
                &today_button,
                &prev_button,
                &next_button,
                &month_toggle,
                &week_toggle,
            );
            glib::ControlFlow::Break
        }
    ));
}

fn connect_handlers(
    ui: &Rc<Ui>,
    today_button: &gtk::Button,
    prev_button: &gtk::Button,
    next_button: &gtk::Button,
    month_toggle: &gtk::ToggleButton,
    week_toggle: &gtk::ToggleButton,
) {
    ui.carousel.connect_page_changed(clone!(
        #[strong]
        ui,
        move |_, index| {
            if ui.rebuilding.get() || (index != 0 && index != 2) {
                return;
            }
            let delta = if index == 0 { -1 } else { 1 };
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(delta);
            drop(s);
            ui.reset();
        }
    ));

    today_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            ui.state.borrow_mut().current_date = Local::now().date_naive();
            ui.reset();
        }
    ));

    prev_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(-1);
            drop(s);
            ui.reset();
        }
    ));

    next_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(1);
            drop(s);
            ui.reset();
        }
    ));

    month_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                ui.state.borrow_mut().view_mode = ViewMode::Month;
                ui.reset();
            }
        }
    ));

    week_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                ui.state.borrow_mut().view_mode = ViewMode::Week;
                ui.reset();
            }
        }
    ));
}

/// Now, rounded up to the next :00 or :30 — a sensible default start time
/// for a brand new event created via the header button (as opposed to
/// clicking a specific day/slot, which uses that exact time instead).
fn next_half_hour() -> DateTime<Local> {
    use chrono::Timelike;
    let now = Local::now();
    let minutes_to_add = 30 - (now.minute() % 30);
    (now + chrono::Duration::minutes(minutes_to_add as i64))
        .with_second(0)
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(now)
}

/// Google API errors carry the full HTML error page as their body; showing
/// all of that in a toast is unreadable (and was actually crashing the
/// toast's markup parser on the `<html lang=e...>` tag). Just the first
/// line — `Google API error (404 Not Found): <!DOCTYPE html>` — is plenty
/// to identify what went wrong.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

fn set_google_button_label(button: &gtk::Button) {
    if google::oauth::has_saved_account() {
        button.set_label("Sync Google");
        button.set_tooltip_text(Some("Click to fetch the latest events from your Google calendars"));
    } else {
        button.set_label("Connect Google");
        button.set_tooltip_text(None);
    }
}

/// If no Google account is connected yet, runs the full OAuth sign-in flow
/// (opens the browser, waits for the redirect, stores the refresh token).
/// Either way, then syncs: fetches every visible Google calendar and its
/// events and upserts them into the local store, so this doubles as
/// "connect" and "sync now" depending on prior state.
///
/// The network/browser-waiting part runs on a background thread — GTK
/// widgets aren't `Send`, so the result comes back over a channel polled
/// from a main-thread timeout rather than being touched directly from
/// that thread. That thread opens its own `Store` (a fresh connection to
/// the same database file) rather than sharing `ui.store`, since
/// `rusqlite::Connection` isn't `Send` either.
fn connect_or_sync_google(ui: &Rc<Ui>, google_button: &gtk::Button) {
    let Some(google_config) = ui.config.google.clone() else {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Add a Google OAuth client to ~/.config/calix/config.toml first — see the README",
        ));
        return;
    };

    let already_connected = google::oauth::has_saved_account();
    google_button.set_sensitive(false);
    google_button.set_label(if already_connected { "Syncing…" } else { "Connecting…" });

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<usize, String> {
            if !already_connected {
                google::oauth::sign_in(&google_config).map_err(|e| e.to_string())?;
            }
            let token = google::oauth::get_access_token(&google_config)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "no access token after sign-in".to_string())?;
            let store = Store::open().map_err(|e| e.to_string())?;
            google::sync::sync(&token, &store)
        })();
        let _ = tx.send(result);
    });

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(
            #[strong]
            ui,
            #[strong]
            google_button,
            move || match rx.try_recv() {
                Ok(Ok(count)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&format!("Synced {count} calendar(s)")));
                    set_google_button_label(&google_button);
                    google_button.set_sensitive(true);
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay.add_toast(adw::Toast::new(&glib::markup_escape_text(
                        &format!("Google sync failed: {}", first_line(&error)),
                    )));
                    set_google_button_label(&google_button);
                    google_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    set_google_button_label(&google_button);
                    google_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}
