use crate::calendar_dialog;
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
    calendar_sidebar: gtk::Box,
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

    fn reset_calendar_sidebar(self: &Rc<Self>) {
        let mut child = self.calendar_sidebar.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.calendar_sidebar.remove(&widget);
            child = next;
        }

        let ui = self.clone();
        self.calendar_sidebar.append(&calendar_dialog::build_list(
            self.store.clone(),
            move || {
                let ui = ui.clone();
                glib::idle_add_local_once(move || {
                    ui.reset();
                    ui.reset_calendar_sidebar();
                });
            },
        ));
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
                event_dialog::open(
                    &ui.carousel,
                    ui.store.clone(),
                    calendar_id,
                    None,
                    start,
                    move || ui_for_saved.reset(),
                );
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
    let calendar_sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_sidebar.set_size_request(340, -1);
    calendar_sidebar.add_css_class("calendar-sidebar");

    let store = Rc::new(Store::open().expect("failed to open Calix's local database"));

    let ui = Rc::new(Ui {
        carousel: carousel.clone(),
        calendar_sidebar: calendar_sidebar.clone(),
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
            event_dialog::open(
                &ui.carousel,
                ui.store.clone(),
                calendar_id,
                None,
                start,
                move || ui2.reset(),
            );
        }
    ));

    let google_sync_button = gtk::Button::with_label("Sync Google");
    update_google_sync_button(&ui, &google_sync_button);
    google_sync_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        google_sync_button,
        move |_| sync_google_accounts(&ui, &google_sync_button)
    ));

    let google_add_button = gtk::Button::with_label("Add Google");
    google_add_button.set_tooltip_text(Some("Connect another Google account"));
    google_add_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        google_add_button,
        #[weak]
        google_sync_button,
        move |_| add_google_account(&ui, &google_add_button, &google_sync_button)
    ));

    ui.reset_calendar_sidebar();

    let calendars_button = gtk::ToggleButton::new();
    calendars_button.set_child(Some(&gtk::Image::from_icon_name(
        "x-office-calendar-symbolic",
    )));
    calendars_button.set_tooltip_text(Some("Show Calendars"));
    calendars_button.set_active(true);
    calendars_button.connect_clicked(clone!(
        #[strong]
        calendar_sidebar,
        move |button| {
            calendar_sidebar.set_visible(button.is_active());
        }
    ));

    let header = adw::HeaderBar::new();
    header.pack_start(&today_button);
    header.pack_start(&nav_box);
    header.set_title_widget(Some(&ui.title_label));
    header.pack_end(&view_toggle_box);
    header.pack_end(&new_event_button);
    header.pack_end(&calendars_button);
    header.pack_end(&google_sync_button);
    header.pack_end(&google_add_button);

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&calendar_sidebar));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_end_child(Some(&carousel));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&paned));

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

fn update_google_sync_button(ui: &Rc<Ui>, button: &gtk::Button) {
    let account_count = ui
        .store
        .google_accounts()
        .map(|accounts| accounts.len())
        .unwrap_or(0);
    button.set_sensitive(true);
    button.set_tooltip_text(if account_count > 0 {
        Some("Fetch the latest events from connected Google accounts")
    } else {
        Some("Fetch calendars from connected Google accounts")
    });
}

struct GoogleAddResult {
    display_name: String,
    calendars_synced: usize,
}

/// Runs the interactive OAuth flow for a new Google account, identifies the
/// signed-in account from its primary calendar, saves that account-specific
/// refresh token, and immediately performs an initial sync.
fn add_google_account(ui: &Rc<Ui>, add_button: &gtk::Button, sync_button: &gtk::Button) {
    let Some(google_config) = ui.config.google.clone() else {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Add a Google OAuth client to ~/.config/calix/config.toml first — see the README",
        ));
        return;
    };

    add_button.set_sensitive(false);
    add_button.set_label("Connecting…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<GoogleAddResult, String> {
            let tokens = google::oauth::sign_in(&google_config).map_err(|e| e.to_string())?;
            let (provider_account_id, display_name) =
                google::sync::account_identity(&tokens.access_token)?;
            let token_key = google::oauth::token_key(&provider_account_id);
            let store = Store::open().map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_google_account(&provider_account_id, &display_name, &token_key)
                .map_err(|e| e.to_string())?;
            google::oauth::save_refresh_token(&token_key, &tokens.refresh_token)
                .map_err(|e| e.to_string())?;
            let calendars_synced =
                google::sync::sync_account(&tokens.access_token, &store, account_id)?;
            Ok(GoogleAddResult {
                display_name,
                calendars_synced,
            })
        })();
        let _ = tx.send(result);
    });

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(
            #[strong]
            ui,
            #[strong]
            add_button,
            #[strong]
            sync_button,
            move || match rx.try_recv() {
                Ok(Ok(result)) => {
                    ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Added {} and synced {} calendar(s)",
                        result.display_name, result.calendars_synced
                    )));
                    add_button.set_label("Add Google");
                    add_button.set_sensitive(true);
                    update_google_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "Google connect failed: {}",
                            first_line(&error)
                        ))));
                    add_button.set_label("Add Google");
                    add_button.set_sensitive(true);
                    update_google_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    add_button.set_label("Add Google");
                    add_button.set_sensitive(true);
                    update_google_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}

/// Syncs every connected Google account. The network work runs on a
/// background thread; the thread opens its own SQLite connection because
/// `Store` wraps a `rusqlite::Connection`, which is not `Send`.
fn sync_google_accounts(ui: &Rc<Ui>, sync_button: &gtk::Button) {
    let Some(google_config) = ui.config.google.clone() else {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Add a Google OAuth client to ~/.config/calix/config.toml first — see the README",
        ));
        return;
    };

    sync_button.set_sensitive(false);
    sync_button.set_label("Syncing…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<(usize, usize), String> {
            let store = Store::open().map_err(|e| e.to_string())?;
            let mut accounts = store.google_accounts().map_err(|e| e.to_string())?;
            if accounts.is_empty() {
                if let Some(token) = google::oauth::get_access_token(
                    &google_config,
                    google::oauth::legacy_token_key(),
                )
                .map_err(|e| e.to_string())?
                {
                    let (provider_account_id, display_name) =
                        google::sync::account_identity(&token)?;
                    let token_key = google::oauth::token_key(&provider_account_id);
                    google::oauth::copy_refresh_token(
                        google::oauth::legacy_token_key(),
                        &token_key,
                    )
                    .map_err(|e| e.to_string())?;
                    store
                        .upsert_google_account(&provider_account_id, &display_name, &token_key)
                        .map_err(|e| e.to_string())?;
                    accounts = store.google_accounts().map_err(|e| e.to_string())?;
                }
            }
            if accounts.is_empty() {
                return Err("No Google accounts connected. Use Add Google first.".to_string());
            }
            let account_count = accounts.len();
            let mut calendar_count = 0;

            for account in accounts {
                let token = google::oauth::get_access_token(&google_config, &account.token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "missing saved token for {} ({})",
                            account.display_name, account.provider_account_id
                        )
                    })?;
                calendar_count += google::sync::sync_account(&token, &store, account.id)?;
            }

            Ok((account_count, calendar_count))
        })();
        let _ = tx.send(result);
    });

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(
            #[strong]
            ui,
            #[strong]
            sync_button,
            move || match rx.try_recv() {
                Ok(Ok((account_count, calendar_count))) => {
                    ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Synced {calendar_count} calendar(s) from {account_count} account(s)"
                    )));
                    sync_button.set_label("Sync Google");
                    update_google_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "Google sync failed: {}",
                            first_line(&error)
                        ))));
                    sync_button.set_label("Sync Google");
                    update_google_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    sync_button.set_label("Sync Google");
                    update_google_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}
