use crate::calendar_dialog;
use crate::config::Config;
use crate::date_util::{
    day_bounds, month_grid_bounds, month_start, shift_days, shift_months, shift_weeks, week_bounds,
    week_dates, week_start,
};
use crate::event_dialog;
use crate::google;
use crate::icloud;
use crate::store::{self, Event, EventDraft, Store};
use crate::views::{drag::DragKind, month_view, week_view};
use adw::prelude::*;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, NaiveTime};
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
    Day,
}

impl ViewMode {
    const SETTING_KEY: &'static str = "view_mode";

    fn from_setting(value: Option<String>) -> Self {
        match value.as_deref() {
            Some("day") => ViewMode::Day,
            Some("week") => ViewMode::Week,
            _ => ViewMode::Month,
        }
    }

    fn as_setting(self) -> &'static str {
        match self {
            ViewMode::Month => "month",
            ViewMode::Week => "week",
            ViewMode::Day => "day",
        }
    }
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
            ViewMode::Day => self.current_date,
        }
    }

    fn shift(&self, delta: i32) -> NaiveDate {
        self.shift_from(self.current_date, delta)
    }

    fn shift_from(&self, date: NaiveDate, delta: i32) -> NaiveDate {
        match self.view_mode {
            ViewMode::Month => shift_months(date, delta),
            ViewMode::Week => shift_weeks(date, delta),
            ViewMode::Day => shift_days(date, delta),
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
            ViewMode::Day => self.current_date.format("%A, %B %-d, %Y").to_string(),
        }
    }
}

/// Bundles the widgets `reset` and the interactive handlers both need, so
/// they don't have to be threaded through as a long parameter list.
struct Ui {
    carousel: adw::Carousel,
    calendar_list: gtk::Box,
    title_label: gtk::Label,
    toast_overlay: adw::ToastOverlay,
    state: Rc<RefCell<State>>,
    store: Rc<Store>,
    config: Rc<Config>,
    rebuilding: Rc<Cell<bool>>,
}

impl Ui {
    /// Clears the carousel and rebuilds it with prev/current/next pages
    /// centered on the selected date.
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
        self.carousel.set_orientation(match view_mode {
            ViewMode::Month => gtk::Orientation::Vertical,
            ViewMode::Week | ViewMode::Day => gtk::Orientation::Horizontal,
        });
        let current_date = state.current_date;
        let prev_date = state.shift_from(current_date, -1);
        let next_date = state.shift_from(current_date, 1);
        let title = state.title();
        drop(state);

        let current_page = self.build_page(view_mode, current_date);
        let prev_page = self.build_page(view_mode, prev_date);
        let next_page = self.build_page(view_mode, next_date);

        self.carousel.append(&prev_page);
        self.carousel.append(&current_page);
        self.carousel.append(&next_page);

        self.title_label.set_label(&title);

        let ui = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            ui.carousel.scroll_to(&current_page, false);
            let ui = ui.clone();
            glib::timeout_add_local_once(Duration::from_millis(50), move || {
                ui.carousel.scroll_to(&current_page, false);
                ui.rebuilding.set(false);
            });
        });
    }

    /// Advances the carousel by one period without rebuilding the page that
    /// is currently visible. Keeping that page attached prevents the flash
    /// caused by clearing the entire carousel at the end of every swipe.
    fn advance(self: &Rc<Self>, delta: i32) {
        self.rebuilding.set(true);

        let mut state = self.state.borrow_mut();
        state.current_date = state.shift(delta);
        let view_mode = state.view_mode;
        let current_date = state.current_date;
        let replacement_date = state.shift_from(current_date, delta);
        let title = state.title();
        drop(state);

        let replacement = self.build_page(view_mode, replacement_date);
        if delta > 0 {
            if let Some(old_prev) = self.carousel.first_child() {
                self.carousel.remove(&old_prev);
            }
            self.carousel.append(&replacement);
        } else {
            if let Some(old_next) = self.carousel.last_child() {
                self.carousel.remove(&old_next);
            }
            self.carousel.insert(&replacement, 0);
        }
        self.title_label.set_label(&title);

        let Some(current_page) = self
            .carousel
            .first_child()
            .and_then(|page| page.next_sibling())
        else {
            self.rebuilding.set(false);
            return;
        };
        // When moving backward, inserting the new previous page briefly puts
        // it at position zero. Recenter before GTK can paint that transient
        // page, otherwise the swipe flashes the wrong week for one frame.
        self.carousel.scroll_to(&current_page, false);
        let ui = self.clone();
        glib::idle_add_local_once(move || {
            ui.rebuilding.set(false);
        });
    }

    fn reset_calendar_sidebar(self: &Rc<Self>) {
        let mut child = self.calendar_list.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.calendar_list.remove(&widget);
            child = next;
        }

        let ui = self.clone();
        self.calendar_list.append(&calendar_dialog::build_list(
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
                let ui_for_saved = ui.clone();
                event_dialog::open(
                    &ui.carousel,
                    ui.store.clone(),
                    create_targets(&ui),
                    None,
                    start,
                    move || ui_for_saved.reset(),
                    None,
                );
            })
        };
        let on_edit: Rc<dyn Fn(Event)> = {
            let ui = self.clone();
            Rc::new(move |event: Event| {
                let start = event.start;
                let ui_for_saved = ui.clone();
                let remote_event = remote_event_handler(&ui, &event);
                event_dialog::open(
                    &ui.carousel,
                    ui.store.clone(),
                    Vec::new(),
                    Some(event),
                    start,
                    move || ui_for_saved.reset(),
                    remote_event,
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
                let on_move = move_handler(self, events.clone());
                month_view::build(date, &events, on_create, on_edit, on_move)
            }
            ViewMode::Week => {
                let (range_start, range_end) = week_bounds(date);
                let events = self
                    .store
                    .events_between(store::day_start(range_start), store::day_start(range_end))
                    .unwrap_or_default();
                let on_move = move_handler(self, events.clone());
                week_view::build(date, &events, on_create, on_edit, on_move)
            }
            ViewMode::Day => {
                let (range_start, range_end) = day_bounds(date);
                let events = self
                    .store
                    .events_between(store::day_start(range_start), store::day_start(range_end))
                    .unwrap_or_default();
                let on_move = move_handler(self, events.clone());
                week_view::build_day(date, &events, on_create, on_edit, on_move)
            }
        }
    }
}

pub fn build(app: &adw::Application) {
    let store = Rc::new(Store::open().expect("failed to open Calix's local database"));
    let initial_view_mode =
        ViewMode::from_setting(store.setting(ViewMode::SETTING_KEY).unwrap_or_default());
    let state = Rc::new(RefCell::new(State {
        view_mode: initial_view_mode,
        current_date: Local::now().date_naive(),
    }));

    let carousel = adw::Carousel::builder()
        .allow_scroll_wheel(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    let calendar_sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_sidebar.set_size_request(300, -1);
    calendar_sidebar.set_visible(false);
    calendar_sidebar.add_css_class("calendar-sidebar");
    let calendar_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_list.set_hexpand(true);
    calendar_list.set_vexpand(true);
    let title_label = gtk::Label::builder().css_classes(["title"]).build();
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title_label.set_width_chars(12);
    title_label.set_max_width_chars(28);

    let ui = Rc::new(Ui {
        carousel: carousel.clone(),
        calendar_list: calendar_list.clone(),
        title_label,
        toast_overlay: adw::ToastOverlay::new(),
        state,
        store,
        config: Rc::new(Config::load()),
        // Guards against `page-changed`/`toggled` firing (and reentering
        // `rebuild`) as a side effect of our own programmatic changes.
        rebuilding: Rc::new(Cell::new(false)),
    });

    let today_button = gtk::Button::builder().label("Today").build();
    today_button.add_css_class("header-small");
    // Header-bar children default to valign fill, which stretches buttons to
    // the bar's full content height — natural (small) height needs center.
    today_button.set_valign(gtk::Align::Center);
    let prev_button = gtk::Button::from_icon_name("go-previous-symbolic");
    let next_button = gtk::Button::from_icon_name("go-next-symbolic");
    let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_box.add_css_class("linked");
    nav_box.append(&prev_button);
    nav_box.append(&next_button);

    let month_toggle = gtk::ToggleButton::builder()
        .label("Month")
        .active(initial_view_mode == ViewMode::Month)
        .build();
    let week_toggle = gtk::ToggleButton::builder()
        .label("Week")
        .group(&month_toggle)
        .active(initial_view_mode == ViewMode::Week)
        .build();
    let day_toggle = gtk::ToggleButton::builder()
        .label("Day")
        .group(&month_toggle)
        .active(initial_view_mode == ViewMode::Day)
        .build();
    let view_toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    view_toggle_box.add_css_class("linked");
    view_toggle_box.append(&month_toggle);
    view_toggle_box.append(&week_toggle);
    view_toggle_box.append(&day_toggle);
    view_toggle_box.set_valign(gtk::Align::Center);
    for toggle in [&month_toggle, &week_toggle, &day_toggle] {
        toggle.add_css_class("header-small");
    }

    let new_event_button = gtk::Button::from_icon_name("list-add-symbolic");
    new_event_button.set_tooltip_text(Some("New Event"));
    new_event_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let start = next_half_hour();
            let ui2 = ui.clone();
            event_dialog::open(
                &ui.carousel,
                ui.store.clone(),
                create_targets(&ui),
                None,
                start,
                move || ui2.reset(),
                None,
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

    let icloud_sync_button = gtk::Button::with_label("Sync iCloud");
    update_icloud_sync_button(&ui, &icloud_sync_button);
    icloud_sync_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        icloud_sync_button,
        move |_| sync_icloud_accounts(&ui, &icloud_sync_button)
    ));

    let icloud_add_button = gtk::Button::with_label("Add iCloud");
    icloud_add_button.set_tooltip_text(Some("Connect an iCloud account"));
    icloud_add_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        icloud_add_button,
        #[weak]
        icloud_sync_button,
        move |_| open_icloud_account_dialog(&ui, &icloud_add_button, &icloud_sync_button)
    ));

    calendar_sidebar.append(&sidebar_actions(
        &google_add_button,
        &google_sync_button,
        &icloud_add_button,
        &icloud_sync_button,
    ));
    calendar_sidebar.append(&calendar_list);
    ui.reset_calendar_sidebar();

    let calendars_button = gtk::ToggleButton::new();
    calendars_button.set_child(Some(&gtk::Image::from_icon_name(
        "x-office-calendar-symbolic",
    )));
    calendars_button.set_tooltip_text(Some("Show Calendars"));
    calendars_button.set_active(false);
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

    // Below this width, step the grid's text down a size (style.rs's
    // `window.compact-text` rules) so day columns stay readable instead of
    // ellipsizing everything away.
    let compact = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        960.0,
        adw::LengthUnit::Sp,
    ));
    compact.connect_apply(clone!(
        #[weak]
        window,
        move |_| window.add_css_class("compact-text")
    ));
    compact.connect_unapply(clone!(
        #[weak]
        window,
        move |_| window.remove_css_class("compact-text")
    ));
    window.add_breakpoint(compact);

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
        #[strong]
        day_toggle,
        move |carousel, _clock| {
            if carousel.width() <= 0 {
                return glib::ControlFlow::Continue;
            }

            ui.state.borrow_mut().current_date = Local::now().date_naive();
            ui.reset();
            glib::timeout_add_local_once(
                Duration::from_millis(125),
                clone!(
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
                    #[strong]
                    day_toggle,
                    move || {
                        connect_handlers(
                            &ui,
                            &today_button,
                            &prev_button,
                            &next_button,
                            &month_toggle,
                            &week_toggle,
                            &day_toggle,
                        );
                    }
                ),
            );
            glib::ControlFlow::Break
        }
    ));
}

fn sidebar_actions(
    google_add_button: &gtk::Button,
    google_sync_button: &gtk::Button,
    icloud_add_button: &gtk::Button,
    icloud_sync_button: &gtk::Button,
) -> gtk::Widget {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.add_css_class("sidebar-actions");
    section.set_margin_top(12);
    section.set_margin_bottom(8);
    section.set_margin_start(12);
    section.set_margin_end(12);

    let title = gtk::Label::new(Some("Accounts"));
    title.add_css_class("caption-heading");
    title.add_css_class("dim-label");
    title.set_xalign(0.0);
    section.append(&title);

    let google_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    google_row.append(google_add_button);
    google_row.append(google_sync_button);
    section.append(&google_row);

    let icloud_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    icloud_row.append(icloud_add_button);
    icloud_row.append(icloud_sync_button);
    section.append(&icloud_row);

    for button in [
        google_add_button,
        google_sync_button,
        icloud_add_button,
        icloud_sync_button,
    ] {
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        button.add_css_class("sidebar-action-button");
    }

    section.upcast()
}

fn connect_handlers(
    ui: &Rc<Ui>,
    today_button: &gtk::Button,
    prev_button: &gtk::Button,
    next_button: &gtk::Button,
    month_toggle: &gtk::ToggleButton,
    week_toggle: &gtk::ToggleButton,
    day_toggle: &gtk::ToggleButton,
) {
    ui.carousel.connect_page_changed(clone!(
        #[strong]
        ui,
        move |_, index| {
            if ui.rebuilding.get() || (index != 0 && index != 2) {
                return;
            }
            let delta = if index == 0 { -1 } else { 1 };
            // `page-changed` is emitted while the swipe animation is still
            // active. Recycling pages here races that animation, most
            // visibly when a new page is inserted before the current one.
            if delta > 0 {
                ui.advance(delta);
            } else {
                ui.rebuilding.set(true);
                let ui = ui.clone();
                glib::timeout_add_local_once(Duration::from_millis(180), move || {
                    ui.advance(delta);
                });
            }
        }
    ));

    today_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let today = Local::now().date_naive();
            ui.state.borrow_mut().current_date = today;
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
                set_view_mode(&ui, ViewMode::Month);
                ui.reset();
            }
        }
    ));

    week_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                set_view_mode(&ui, ViewMode::Week);
                ui.reset();
            }
        }
    ));

    day_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                set_view_mode(&ui, ViewMode::Day);
                ui.reset();
            }
        }
    ));
}

fn set_view_mode(ui: &Rc<Ui>, view_mode: ViewMode) {
    ui.state.borrow_mut().view_mode = view_mode;
    let _ = ui
        .store
        .set_setting(ViewMode::SETTING_KEY, view_mode.as_setting());
}

fn remote_event_handler(ui: &Rc<Ui>, event: &Event) -> Option<event_dialog::RemoteEvent> {
    match event.account_provider.as_deref() {
        Some("google") => {
            let Some(config) = ui.config.google.clone() else {
                return Some(event_dialog::RemoteEvent::Unavailable(
                    "Google is not configured on this machine".to_string(),
                ));
            };
            let (Some(token_key), Some(calendar_id), Some(event_id)) = (
                event.account_token_key.clone(),
                event.google_calendar_id.clone(),
                event.google_event_id.clone(),
            ) else {
                return Some(event_dialog::RemoteEvent::Unavailable(
                    "This Google event is missing sync metadata".to_string(),
                ));
            };
            Some(event_dialog::RemoteEvent::Google {
                config,
                token_key,
                calendar_id,
                event_id,
            })
        }
        Some("icloud") => {
            let (Some(apple_id), Some(token_key), Some(event_href)) = (
                event.account_provider_id.clone(),
                event.account_token_key.clone(),
                event.icloud_event_id.clone(),
            ) else {
                return Some(event_dialog::RemoteEvent::Unavailable(
                    "This iCloud event is missing sync metadata".to_string(),
                ));
            };
            Some(event_dialog::RemoteEvent::Icloud {
                apple_id,
                token_key,
                event_href,
            })
        }
        _ => None,
    }
}

fn create_targets(ui: &Rc<Ui>) -> Vec<event_dialog::TargetChoice> {
    ui.store
        .calendar_connections()
        .unwrap_or_default()
        .into_iter()
        .map(|calendar| {
            let visible = calendar.visible;
            let target = match calendar.provider.as_deref() {
                Some("google") => match (
                    ui.config.google.clone(),
                    calendar.token_key,
                    calendar.google_calendar_id,
                ) {
                    (Some(config), Some(token_key), Some(google_calendar_id)) => {
                        event_dialog::CreateTarget::Google {
                            calendar_id: calendar.id,
                            name: calendar.name,
                            config,
                            token_key,
                            google_calendar_id,
                        }
                    }
                    _ => event_dialog::CreateTarget::Unavailable {
                        calendar_id: calendar.id,
                        name: calendar.name,
                        error: "Google calendar is not configured on this machine".to_string(),
                    },
                },
                Some("icloud") => match (
                    calendar.provider_account_id,
                    calendar.token_key,
                    calendar.icloud_calendar_id,
                ) {
                    (Some(apple_id), Some(token_key), Some(calendar_href)) => {
                        event_dialog::CreateTarget::Icloud {
                            calendar_id: calendar.id,
                            name: calendar.name,
                            apple_id,
                            token_key,
                            calendar_href,
                        }
                    }
                    _ => event_dialog::CreateTarget::Unavailable {
                        calendar_id: calendar.id,
                        name: calendar.name,
                        error: "iCloud calendar is missing sync metadata".to_string(),
                    },
                },
                _ => event_dialog::CreateTarget::Local {
                    calendar_id: calendar.id,
                    name: calendar.name,
                },
            };
            event_dialog::TargetChoice { target, visible }
        })
        .collect()
}

fn move_handler(
    ui: &Rc<Ui>,
    events: Vec<Event>,
) -> Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)> {
    let ui = ui.clone();
    Rc::new(move |kind, event_id, target_date, target_time| {
        let Some(event) = events.iter().find(|event| event.id == event_id).cloned() else {
            return;
        };
        let Some(draft) = drag_draft(&event, kind, target_date, target_time) else {
            ui.toast_overlay.add_toast(adw::Toast::new(
                "Resize needs a timed slot in week or day view",
            ));
            return;
        };
        let original = event_to_draft(&event);
        match remote_event_handler(&ui, &event) {
            Some(event_dialog::RemoteEvent::Unavailable(error)) => {
                ui.toast_overlay.add_toast(adw::Toast::new(&error));
            }
            Some(remote_event) => {
                if let Err(error) = ui.store.update_event(event.id, &draft) {
                    ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Couldn't move event locally: {error}"
                    )));
                    return;
                }
                ui.reset();

                let (tx, rx) = mpsc::channel();
                let remote_draft = draft.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(remote_event.update(&remote_draft));
                });
                glib::timeout_add_local(
                    Duration::from_millis(100),
                    clone!(
                        #[strong]
                        ui,
                        move || match rx.try_recv() {
                            Ok(Ok(())) => glib::ControlFlow::Break,
                            Ok(Err(error)) => {
                                let _ = ui.store.update_event(event.id, &original);
                                ui.reset();
                                ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                                    "Couldn't move event: {error}"
                                )));
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                let _ = ui.store.update_event(event.id, &original);
                                ui.reset();
                                ui.toast_overlay
                                    .add_toast(adw::Toast::new("Event move stopped unexpectedly"));
                                glib::ControlFlow::Break
                            }
                        }
                    ),
                );
            }
            None => {
                if let Err(error) = ui.store.update_event(event.id, &draft) {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&format!("Couldn't move event: {error}")));
                } else {
                    ui.reset();
                }
            }
        }
    })
}

fn event_to_draft(event: &Event) -> EventDraft {
    EventDraft {
        title: event.title.clone(),
        start: event.start,
        end: event.end,
        all_day: event.all_day,
        location: event.location.clone(),
        notes: event.notes.clone(),
    }
}

fn drag_draft(
    event: &Event,
    kind: DragKind,
    target_date: NaiveDate,
    target_time: Option<NaiveTime>,
) -> Option<EventDraft> {
    match kind {
        DragKind::Move => Some(moved_draft(event, target_date, target_time)),
        DragKind::ResizeStart => resized_start_draft(event, target_date, target_time),
        DragKind::ResizeEnd => resized_end_draft(event, target_date, target_time),
    }
}

fn moved_draft(
    event: &Event,
    target_date: NaiveDate,
    target_time: Option<NaiveTime>,
) -> EventDraft {
    let duration = event.end - event.start;
    let start = target_date
        .and_time(target_time.unwrap_or_else(|| event.start.time()))
        .and_local_timezone(Local)
        .single()
        .unwrap_or(event.start);
    EventDraft {
        title: event.title.clone(),
        start,
        end: start + duration,
        all_day: event.all_day,
        location: event.location.clone(),
        notes: event.notes.clone(),
    }
}

fn resized_start_draft(
    event: &Event,
    target_date: NaiveDate,
    target_time: Option<NaiveTime>,
) -> Option<EventDraft> {
    if event.all_day {
        return None;
    }
    let target_time = target_time?;
    let new_start = target_date
        .and_time(target_time)
        .and_local_timezone(Local)
        .single()?;
    // Matches the interactive resize's 15-minute floor (drag::MIN_BLOCK_MINUTES)
    // so a committed resize never snaps away from its preview.
    let latest_start = event.end - ChronoDuration::minutes(15);
    let start = new_start.min(latest_start);
    Some(EventDraft {
        title: event.title.clone(),
        start,
        end: event.end,
        all_day: event.all_day,
        location: event.location.clone(),
        notes: event.notes.clone(),
    })
}

fn resized_end_draft(
    event: &Event,
    target_date: NaiveDate,
    target_time: Option<NaiveTime>,
) -> Option<EventDraft> {
    if event.all_day {
        return None;
    }
    let target_time = target_time?;
    let new_end = target_date
        .and_time(target_time)
        .and_local_timezone(Local)
        .single()?;
    let earliest_end = event.start + ChronoDuration::minutes(15);
    let end = new_end.max(earliest_end);
    Some(EventDraft {
        title: event.title.clone(),
        start: event.start,
        end,
        all_day: event.all_day,
        location: event.location.clone(),
        notes: event.notes.clone(),
    })
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

fn update_icloud_sync_button(ui: &Rc<Ui>, button: &gtk::Button) {
    let account_count = ui
        .store
        .icloud_accounts()
        .map(|accounts| accounts.len())
        .unwrap_or(0);
    button.set_sensitive(true);
    button.set_tooltip_text(if account_count > 0 {
        Some("Fetch the latest events from connected iCloud accounts")
    } else {
        Some("Fetch calendars from connected iCloud accounts")
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

struct IcloudAddResult {
    display_name: String,
    calendars_synced: usize,
}

fn open_icloud_account_dialog(ui: &Rc<Ui>, add_button: &gtk::Button, sync_button: &gtk::Button) {
    let dialog = adw::Dialog::builder()
        .title("Add iCloud")
        .content_width(420)
        .build();

    let cancel_button = gtk::Button::with_label("Cancel");
    let connect_button = gtk::Button::builder()
        .label("Connect")
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_button);
    header.pack_end(&connect_button);

    let apple_id_row = adw::EntryRow::builder()
        .title("Apple Account Email")
        .build();
    let password_row = adw::PasswordEntryRow::builder()
        .title("App-Specific Password")
        .build();

    let group = adw::PreferencesGroup::new();
    group.add(&apple_id_row);
    group.add(&password_row);

    let note = gtk::Label::new(Some(
        "Use an app-specific password from account.apple.com, not your Apple Account password.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&group);
    content.append(&note);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    connect_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[strong]
        add_button,
        #[strong]
        sync_button,
        #[weak]
        dialog,
        move |_| {
            let apple_id = apple_id_row.text().trim().to_string();
            let app_password = password_row.text().trim().to_string();
            if apple_id.is_empty() || app_password.is_empty() {
                return;
            }
            dialog.close();
            add_icloud_account(&ui, &add_button, &sync_button, apple_id, app_password);
        }
    ));

    dialog.present(Some(&ui.carousel));
}

fn add_icloud_account(
    ui: &Rc<Ui>,
    add_button: &gtk::Button,
    sync_button: &gtk::Button,
    apple_id: String,
    app_password: String,
) {
    add_button.set_sensitive(false);
    add_button.set_label("Connecting…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<IcloudAddResult, String> {
            let credentials = icloud::caldav::Credentials {
                apple_id: apple_id.clone(),
                app_password,
            };
            icloud::caldav::discover_calendars(&credentials)?;

            let token_key = icloud::credentials::token_key(&apple_id);
            let store = Store::open().map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_icloud_account(&apple_id, &apple_id, &token_key)
                .map_err(|e| e.to_string())?;
            icloud::credentials::save_app_password(&token_key, &credentials.app_password)
                .map_err(|e| e.to_string())?;
            let calendars_synced = icloud::sync::sync_account(&credentials, &store, account_id)?;
            Ok(IcloudAddResult {
                display_name: apple_id,
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
                        "Added {} and synced {} iCloud calendar(s)",
                        result.display_name, result.calendars_synced
                    )));
                    add_button.set_label("Add iCloud");
                    add_button.set_sensitive(true);
                    update_icloud_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "iCloud connect failed: {}",
                            first_line(&error)
                        ))));
                    add_button.set_label("Add iCloud");
                    add_button.set_sensitive(true);
                    update_icloud_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    add_button.set_label("Add iCloud");
                    add_button.set_sensitive(true);
                    update_icloud_sync_button(&ui, &sync_button);
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
            if accounts.is_empty()
                && let Some(token) = google::oauth::get_access_token(
                    &google_config,
                    google::oauth::legacy_token_key(),
                )
                .map_err(|e| e.to_string())?
            {
                let (provider_account_id, display_name) = google::sync::account_identity(&token)?;
                let token_key = google::oauth::token_key(&provider_account_id);
                google::oauth::copy_refresh_token(google::oauth::legacy_token_key(), &token_key)
                    .map_err(|e| e.to_string())?;
                store
                    .upsert_google_account(&provider_account_id, &display_name, &token_key)
                    .map_err(|e| e.to_string())?;
                accounts = store.google_accounts().map_err(|e| e.to_string())?;
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

fn sync_icloud_accounts(ui: &Rc<Ui>, sync_button: &gtk::Button) {
    sync_button.set_sensitive(false);
    sync_button.set_label("Syncing…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<(usize, usize), String> {
            let store = Store::open().map_err(|e| e.to_string())?;
            let accounts = store.icloud_accounts().map_err(|e| e.to_string())?;
            if accounts.is_empty() {
                return Err("No iCloud accounts connected. Use Add iCloud first.".to_string());
            }

            let account_count = accounts.len();
            let mut calendar_count = 0;
            for account in accounts {
                let app_password = icloud::credentials::app_password(&account.token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "missing saved app-specific password for {}",
                            account.display_name
                        )
                    })?;
                let credentials = icloud::caldav::Credentials {
                    apple_id: account.provider_account_id.clone(),
                    app_password,
                };
                calendar_count += icloud::sync::sync_account(&credentials, &store, account.id)?;
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
                        "Synced {calendar_count} iCloud calendar(s) from {account_count} account(s)"
                    )));
                    sync_button.set_label("Sync iCloud");
                    update_icloud_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "iCloud sync failed: {}",
                            first_line(&error)
                        ))));
                    sync_button.set_label("Sync iCloud");
                    update_icloud_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    sync_button.set_label("Sync iCloud");
                    sync_button.set_sensitive(true);
                    update_icloud_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}
