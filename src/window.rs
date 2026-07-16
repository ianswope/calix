use crate::caldav;
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

type CreateFn = Rc<dyn Fn(DateTime<Local>)>;
type EditFn = Rc<dyn Fn(Event)>;
type MoveFn = Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>;

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

/// Persisted key for the timed-grid zoom (the pixel height of one hour row in
/// day and week views).
const ZOOM_SETTING_KEY: &str = "hour_row_height";

/// Reads the saved zoom, clamped to the valid range, falling back to the
/// default if it's absent or unparseable.
fn load_hour_row_height(store: &Store) -> i32 {
    store
        .setting(ZOOM_SETTING_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i32>().ok())
        .map(clamp_hour_row_height)
        .unwrap_or(week_view::DEFAULT_HOUR_ROW_HEIGHT)
}

fn clamp_hour_row_height(height: i32) -> i32 {
    height.clamp(
        week_view::MIN_HOUR_ROW_HEIGHT,
        week_view::MAX_HOUR_ROW_HEIGHT,
    )
}

struct State {
    view_mode: ViewMode,
    current_date: NaiveDate,
    hour_row_height: i32,
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
    // The calendar date the display is currently anchored to. A periodic clock
    // tick compares it against the real date so a rollover (left open
    // overnight, or crossed while the machine was suspended) can be noticed and
    // the "today" highlighting re-anchored.
    today: Rc<Cell<NaiveDate>>,
    rebuilding: Rc<Cell<bool>>,
    // Set when a zoom updated only the visible page in place, leaving the
    // offscreen neighbor pages at the old height. The next swipe rebuilds
    // everything (via `reset`) instead of recycling a stale neighbor.
    zoom_dirty: Rc<Cell<bool>>,
}

impl Ui {
    /// Clears the carousel and rebuilds it with prev/current/next pages
    /// centered on the selected date, landing on the usual "now" scroll spot.
    fn reset(self: &Rc<Self>) {
        self.reset_with(week_view::InitialScroll::NowOrMorning);
    }

    /// `reset`, but landing the timed grid at `scroll` — used to keep the same
    /// time in view when a full rebuild happens for reasons other than
    /// navigation (e.g. the first swipe after an in-place zoom).
    fn reset_with(self: &Rc<Self>, scroll: week_view::InitialScroll) {
        self.rebuilding.set(true);
        // A full rebuild makes every page current again.
        self.zoom_dirty.set(false);

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

        let current_page = self.build_page(view_mode, current_date, scroll);
        let prev_page = self.build_page(view_mode, prev_date, scroll);
        let next_page = self.build_page(view_mode, next_date, scroll);

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
        drop(state);

        // A zoom left the neighbor pages at the old height; recycling one as
        // the new current page would show the wrong zoom. Rebuild all three
        // instead, keeping the time the user was looking at (reset clears the
        // flag).
        if self.zoom_dirty.get() {
            let scroll = self
                .visible_scroll_hours()
                .map(week_view::InitialScroll::AtHour)
                .unwrap_or(week_view::InitialScroll::NowOrMorning);
            self.reset_with(scroll);
            return;
        }

        let state = self.state.borrow();
        let view_mode = state.view_mode;
        let current_date = state.current_date;
        let replacement_date = state.shift_from(current_date, delta);
        let title = state.title();
        drop(state);

        let replacement = self.build_page(
            view_mode,
            replacement_date,
            week_view::InitialScroll::NowOrMorning,
        );
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

    /// Runs on a periodic timer to keep the display anchored to real time.
    /// Slides the "now" line to the current time, and when the calendar date
    /// has rolled over re-anchors "today": the highlighting always follows the
    /// real day, and if the user is still parked on today the visible page
    /// follows too. Because GLib timeouts fire promptly once the machine wakes,
    /// this also recovers from a day boundary crossed during suspend.
    fn tick_clock(self: &Rc<Self>) {
        let now_date = Local::now().date_naive();
        let previous = self.today.get();

        // Don't disturb an in-progress swipe/rebuild; the next tick retries
        // (with `today` still unchanged, so the rollover isn't lost).
        if now_date != previous && !self.rebuilding.get() {
            let parked_on_today = self.state.borrow().current_date == previous;
            self.today.set(now_date);
            if parked_on_today {
                self.state.borrow_mut().current_date = now_date;
                self.reset();
                return;
            }
        }

        self.refresh_now_line();
    }

    /// Slides every "now" indicator currently in the carousel to the current
    /// time of day, in place — no rebuild, so the user's scroll position and
    /// swipe are untouched.
    fn refresh_now_line(&self) {
        let hour_row_height = self.state.borrow().hour_row_height;
        let margin = week_view::now_indicator_margin_top(hour_row_height);
        let mut child = self.carousel.first_child();
        while let Some(page) = child {
            move_now_indicators(&page, margin);
            child = page.next_sibling();
        }
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

    /// The create/edit/move callbacks a timed or month page is wired up with:
    /// clicking empty space opens a new-event dialog, clicking an event opens
    /// it, and dragging commits a move/resize. `events` is this page's event
    /// set, which the move handler needs to resolve a drag back to its event.
    fn event_callbacks(self: &Rc<Self>, events: Vec<Event>) -> (CreateFn, EditFn, MoveFn) {
        let on_create: CreateFn = {
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
        let on_edit: EditFn = {
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
        let on_move = move_handler(self, events);
        (on_create, on_edit, on_move)
    }

    /// Builds one page (month grid or week/day grid) for `date`, wired up to
    /// query this page's events from the store and to open the event dialog on
    /// create/edit clicks. `initial_scroll` only matters for timed views.
    fn build_page(
        self: &Rc<Self>,
        view_mode: ViewMode,
        date: NaiveDate,
        initial_scroll: week_view::InitialScroll,
    ) -> gtk::Widget {
        let (range_start, range_end) = match view_mode {
            ViewMode::Month => month_grid_bounds(date),
            ViewMode::Week => week_bounds(date),
            ViewMode::Day => day_bounds(date),
        };
        let events = self
            .store
            .events_between(store::day_start(range_start), store::day_start(range_end))
            .unwrap_or_default();
        let (on_create, on_edit, on_move) = self.event_callbacks(events.clone());

        match view_mode {
            ViewMode::Month => month_view::build(date, &events, on_create, on_edit, on_move),
            ViewMode::Week => {
                let hour_row_height = self.state.borrow().hour_row_height;
                week_view::build(
                    date,
                    &events,
                    on_create,
                    on_edit,
                    on_move,
                    hour_row_height,
                    initial_scroll,
                )
            }
            ViewMode::Day => {
                let hour_row_height = self.state.borrow().hour_row_height;
                week_view::build_day(
                    date,
                    &events,
                    on_create,
                    on_edit,
                    on_move,
                    hour_row_height,
                    initial_scroll,
                )
            }
        }
    }

    /// The `ScrolledWindow` of the currently visible (middle) page, if it's a
    /// timed view. It's the page root's last child (below the header and
    /// all-day rows).
    fn visible_scrolled(&self) -> Option<gtk::ScrolledWindow> {
        let page = self.carousel.first_child()?.next_sibling()?;
        page.last_child().and_downcast::<gtk::ScrolledWindow>()
    }

    /// The fractional hour currently at the top of the visible timed page,
    /// derived from its scroll offset and the current hour height.
    fn visible_scroll_hours(&self) -> Option<f64> {
        let scrolled = self.visible_scrolled()?;
        let height = self.state.borrow().hour_row_height;
        (height > 0).then(|| scrolled.vadjustment().value() / height as f64)
    }

    /// Re-renders just the visible page's hour grid at `new_height`, reusing
    /// its scroll container and keeping the same time at the top of the
    /// viewport. This is the cheap, flash-free path that a live pinch drives
    /// on every frame — no carousel surgery, no full rebuild. The offscreen
    /// neighbor pages are left stale until `refresh_neighbor_pages`.
    fn zoom_visible_page(self: &Rc<Self>, new_height: i32) {
        let (view_mode, date, old_height) = {
            let state = self.state.borrow();
            (state.view_mode, state.current_date, state.hour_row_height)
        };
        if view_mode == ViewMode::Month || new_height == old_height {
            return;
        }
        let Some(scrolled) = self.visible_scrolled() else {
            return;
        };
        let vadj = scrolled.vadjustment();
        let top_hours = if old_height > 0 {
            vadj.value() / old_height as f64
        } else {
            0.0
        };

        let (days, range) = match view_mode {
            ViewMode::Week => (week_dates(date).to_vec(), week_bounds(date)),
            _ => (vec![date], day_bounds(date)),
        };
        let events = self
            .store
            .events_between(store::day_start(range.0), store::day_start(range.1))
            .unwrap_or_default();
        let (on_create, on_edit, on_move) = self.event_callbacks(events.clone());
        let grid =
            week_view::build_hour_grid(&days, &events, on_create, on_edit, on_move, new_height);
        scrolled.set_child(Some(&grid));

        // Set the adjustment to the same time synchronously so the new grid
        // paints in place on its first frame instead of flashing at midnight
        // and then jumping (which is what an idle-deferred scroll would do).
        let upper = (24 * new_height) as f64;
        vadj.set_upper(upper.max(vadj.page_size()));
        vadj.set_value(
            (top_hours * new_height as f64).clamp(0.0, (upper - vadj.page_size()).max(0.0)),
        );

        self.state.borrow_mut().hour_row_height = new_height;
        // The neighbor pages are now stale; the next swipe will rebuild.
        self.zoom_dirty.set(true);
    }
}

pub fn build(app: &adw::Application) {
    let store = Rc::new(Store::open().expect("failed to open Calix's local database"));
    let initial_view_mode =
        ViewMode::from_setting(store.setting(ViewMode::SETTING_KEY).unwrap_or_default());
    let initial_hour_row_height = load_hour_row_height(&store);
    let state = Rc::new(RefCell::new(State {
        view_mode: initial_view_mode,
        current_date: Local::now().date_naive(),
        hour_row_height: initial_hour_row_height,
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
        today: Rc::new(Cell::new(Local::now().date_naive())),
        // Guards against `page-changed`/`toggled` firing (and reentering
        // `rebuild`) as a side effect of our own programmatic changes.
        rebuilding: Rc::new(Cell::new(false)),
        zoom_dirty: Rc::new(Cell::new(false)),
    });

    // Keep the display anchored to real time: slide the "now" line and, on a
    // date rollover, re-anchor "today". A half-minute cadence keeps the line
    // reasonably fresh and bounds how long a suspend/resume rollover can linger
    // — GLib's monotonic timeout fires promptly once the machine wakes.
    glib::timeout_add_seconds_local(
        30,
        clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                ui.tick_clock();
                glib::ControlFlow::Continue
            }
        ),
    );

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

    // Stretch/compress the visible day in week and day views. Hidden in
    // month view, where there is no timed grid to zoom.
    let zoom_out_button = gtk::Button::from_icon_name("zoom-out-symbolic");
    zoom_out_button.set_tooltip_text(Some("Compress the day — fit more hours on screen"));
    let zoom_in_button = gtk::Button::from_icon_name("zoom-in-symbolic");
    zoom_in_button.set_tooltip_text(Some("Stretch the day out — show finer detail"));
    let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    zoom_box.add_css_class("linked");
    zoom_box.append(&zoom_out_button);
    zoom_box.append(&zoom_in_button);
    zoom_box.set_valign(gtk::Align::Center);
    for button in [&zoom_out_button, &zoom_in_button] {
        button.add_css_class("header-small");
    }
    refresh_zoom_controls(&ui, &zoom_box, &zoom_out_button, &zoom_in_button);

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
        move |_| sync_google_accounts(&ui, &google_sync_button, false)
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
        move |_| sync_icloud_accounts(&ui, &icloud_sync_button, false)
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

    let caldav_sync_button = gtk::Button::with_label("Sync CalDAV");
    update_caldav_sync_button(&ui, &caldav_sync_button);
    caldav_sync_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        caldav_sync_button,
        move |_| sync_caldav_accounts(&ui, &caldav_sync_button, false)
    ));

    let caldav_add_button = gtk::Button::with_label("Add CalDAV");
    caldav_add_button.set_tooltip_text(Some(
        "Connect any CalDAV server (Fastmail, Nextcloud, Radicale, …)",
    ));
    caldav_add_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        caldav_add_button,
        #[weak]
        caldav_sync_button,
        move |_| open_caldav_account_dialog(&ui, &caldav_add_button, &caldav_sync_button)
    ));

    calendar_sidebar.append(&sidebar_actions(
        &google_add_button,
        &google_sync_button,
        &icloud_add_button,
        &icloud_sync_button,
        &caldav_add_button,
        &caldav_sync_button,
    ));
    calendar_sidebar.append(&calendar_list);
    ui.reset_calendar_sidebar();

    // Refresh from every connected account as soon as the window is up, then
    // keep the grid fresh with a periodic background re-sync while the app
    // stays open. Both passes are quiet on success (errors still toast) so they
    // don't nag; `sync_connected_accounts` touches only providers that have an
    // account and aren't already mid-sync. The launch pass is deferred a beat
    // so the window paints first.
    glib::timeout_add_local_once(
        Duration::from_millis(100),
        clone!(
            #[strong]
            ui,
            #[weak]
            google_sync_button,
            #[weak]
            icloud_sync_button,
            #[weak]
            caldav_sync_button,
            move || {
                sync_connected_accounts(
                    &ui,
                    &google_sync_button,
                    &icloud_sync_button,
                    &caldav_sync_button,
                );
            }
        ),
    );
    glib::timeout_add_seconds_local(
        15 * 60,
        clone!(
            #[weak]
            ui,
            #[weak]
            google_sync_button,
            #[weak]
            icloud_sync_button,
            #[weak]
            caldav_sync_button,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                sync_connected_accounts(
                    &ui,
                    &google_sync_button,
                    &icloud_sync_button,
                    &caldav_sync_button,
                );
                glib::ControlFlow::Continue
            }
        ),
    );

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
    header.pack_end(&zoom_box);
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
        #[strong]
        zoom_box,
        #[strong]
        zoom_out_button,
        #[strong]
        zoom_in_button,
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
                    #[strong]
                    zoom_box,
                    #[strong]
                    zoom_out_button,
                    #[strong]
                    zoom_in_button,
                    move || {
                        connect_handlers(
                            &ui,
                            &today_button,
                            &prev_button,
                            &next_button,
                            &month_toggle,
                            &week_toggle,
                            &day_toggle,
                            &zoom_box,
                            &zoom_out_button,
                            &zoom_in_button,
                        );
                    }
                ),
            );
            glib::ControlFlow::Break
        }
    ));
}

#[allow(clippy::too_many_arguments)]
fn sidebar_actions(
    google_add_button: &gtk::Button,
    google_sync_button: &gtk::Button,
    icloud_add_button: &gtk::Button,
    icloud_sync_button: &gtk::Button,
    caldav_add_button: &gtk::Button,
    caldav_sync_button: &gtk::Button,
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

    let caldav_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    caldav_row.append(caldav_add_button);
    caldav_row.append(caldav_sync_button);
    section.append(&caldav_row);

    for button in [
        google_add_button,
        google_sync_button,
        icloud_add_button,
        icloud_sync_button,
        caldav_add_button,
        caldav_sync_button,
    ] {
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        button.add_css_class("sidebar-action-button");
    }

    section.upcast()
}

#[allow(clippy::too_many_arguments)]
fn connect_handlers(
    ui: &Rc<Ui>,
    today_button: &gtk::Button,
    prev_button: &gtk::Button,
    next_button: &gtk::Button,
    month_toggle: &gtk::ToggleButton,
    week_toggle: &gtk::ToggleButton,
    day_toggle: &gtk::ToggleButton,
    zoom_box: &gtk::Box,
    zoom_out_button: &gtk::Button,
    zoom_in_button: &gtk::Button,
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

    for (toggle, mode) in [
        (month_toggle, ViewMode::Month),
        (week_toggle, ViewMode::Week),
        (day_toggle, ViewMode::Day),
    ] {
        toggle.connect_toggled(clone!(
            #[strong]
            ui,
            #[strong]
            zoom_box,
            #[strong]
            zoom_out_button,
            #[strong]
            zoom_in_button,
            move |btn| {
                if btn.is_active() {
                    set_view_mode(&ui, mode);
                    ui.reset();
                    refresh_zoom_controls(&ui, &zoom_box, &zoom_out_button, &zoom_in_button);
                }
            }
        ));
    }

    zoom_out_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[strong]
        zoom_box,
        #[strong]
        zoom_out_button,
        #[strong]
        zoom_in_button,
        move |_| {
            adjust_zoom(&ui, -1);
            refresh_zoom_controls(&ui, &zoom_box, &zoom_out_button, &zoom_in_button);
        }
    ));

    zoom_in_button.connect_clicked(clone!(
        #[strong]
        ui,
        #[strong]
        zoom_box,
        #[strong]
        zoom_out_button,
        #[strong]
        zoom_in_button,
        move |_| {
            adjust_zoom(&ui, 1);
            refresh_zoom_controls(&ui, &zoom_box, &zoom_out_button, &zoom_in_button);
        }
    ));

    // Trackpad pinch-to-zoom (and two-finger touch), like Apple Calendar. The
    // gesture lives on the carousel, which outlives page rebuilds, so it stays
    // attached across reset(). A pinch scales the hour height continuously,
    // relative to where it began. Each frame re-renders only the visible page's
    // grid in place — cheap and flash-free, keeping the pinched-around time
    // fixed — while the offscreen neighbor pages and the saved setting are only
    // reconciled once, when the gesture ends.
    let pinch = gtk::GestureZoom::new();
    let pinch_base_height = Rc::new(Cell::new(0i32));
    pinch.connect_begin(clone!(
        #[strong]
        ui,
        #[strong]
        pinch_base_height,
        move |_, _| pinch_base_height.set(ui.state.borrow().hour_row_height)
    ));
    pinch.connect_scale_changed(clone!(
        #[strong]
        ui,
        #[strong]
        pinch_base_height,
        #[strong]
        zoom_box,
        #[strong]
        zoom_out_button,
        #[strong]
        zoom_in_button,
        move |_, scale| {
            let base = pinch_base_height.get();
            if base == 0 || ui.state.borrow().view_mode == ViewMode::Month {
                return;
            }
            let target = clamp_hour_row_height((base as f64 * scale).round() as i32);
            if target != ui.state.borrow().hour_row_height {
                ui.zoom_visible_page(target);
                refresh_zoom_controls(&ui, &zoom_box, &zoom_out_button, &zoom_in_button);
            }
        }
    ));
    pinch.connect_end(clone!(
        #[strong]
        ui,
        #[strong]
        pinch_base_height,
        move |_, _| {
            if pinch_base_height.get() == 0 || ui.state.borrow().view_mode == ViewMode::Month {
                return;
            }
            // The visible page was re-zoomed live; just persist the result.
            // Neighbor pages stay stale until the next swipe rebuilds them.
            let height = ui.state.borrow().hour_row_height;
            let _ = ui.store.set_setting(ZOOM_SETTING_KEY, &height.to_string());
        }
    ));
    ui.carousel.add_controller(pinch);
}

/// How much one zoom-button press changes the hour height, in px.
const ZOOM_BUTTON_STEP: i32 = 12;

/// Depth-first, sets `margin` on every "now" indicator in the subtree. The
/// indicator has no indicator descendants, so its subtree isn't recursed into.
fn move_now_indicators(widget: &gtk::Widget, margin: i32) {
    if widget.widget_name().as_str() == week_view::NOW_INDICATOR_WIDGET_NAME {
        widget.set_margin_top(margin);
        return;
    }
    let mut child = widget.first_child();
    while let Some(node) = child {
        move_now_indicators(&node, margin);
        child = node.next_sibling();
    }
}

/// Sync the zoom control's visibility (week and day views only) and each
/// button's sensitivity (disabled once the smallest/largest height is reached)
/// to the current state.
fn refresh_zoom_controls(
    ui: &Rc<Ui>,
    zoom_box: &gtk::Box,
    zoom_out_button: &gtk::Button,
    zoom_in_button: &gtk::Button,
) {
    let state = ui.state.borrow();
    zoom_box.set_visible(state.view_mode != ViewMode::Month);
    zoom_out_button.set_sensitive(state.hour_row_height > week_view::MIN_HOUR_ROW_HEIGHT);
    zoom_in_button.set_sensitive(state.hour_row_height < week_view::MAX_HOUR_ROW_HEIGHT);
}

/// Steps the zoom by `steps` button-steps (negative compresses, positive
/// stretches): re-render the visible page in place and persist. A no-op once
/// the end of the range is reached.
fn adjust_zoom(ui: &Rc<Ui>, steps: i32) {
    let current = ui.state.borrow().hour_row_height;
    let target = clamp_hour_row_height(current + steps * ZOOM_BUTTON_STEP);
    if target == current {
        return;
    }
    ui.zoom_visible_page(target);
    let _ = ui.store.set_setting(ZOOM_SETTING_KEY, &target.to_string());
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
        Some(provider @ ("icloud" | "caldav")) => {
            let (Some(username), Some(token_key), Some(event_href)) = (
                event.account_provider_id.clone(),
                event.account_token_key.clone(),
                event.icloud_event_id.clone(),
            ) else {
                return Some(event_dialog::RemoteEvent::Unavailable(
                    "This event is missing sync metadata".to_string(),
                ));
            };
            let base_url = match caldav_base_url(provider, event.account_server_url.as_deref()) {
                Ok(base_url) => base_url,
                Err(error) => return Some(event_dialog::RemoteEvent::Unavailable(error)),
            };
            Some(event_dialog::RemoteEvent::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            })
        }
        _ => None,
    }
}

/// The CalDAV base URL for a synced calendar/event: iCloud's fixed root, or a
/// generic account's stored `server_url`.
fn caldav_base_url(provider: &str, server_url: Option<&str>) -> Result<String, String> {
    match provider {
        "icloud" => Ok(icloud::ICLOUD_CALDAV_ROOT.to_string()),
        _ => server_url
            .map(str::to_string)
            .ok_or_else(|| "This CalDAV account is missing its server address".to_string()),
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
                Some(provider @ ("icloud" | "caldav")) => match (
                    calendar.provider_account_id,
                    calendar.token_key,
                    calendar.icloud_calendar_id,
                    caldav_base_url(provider, calendar.server_url.as_deref()),
                ) {
                    (Some(username), Some(token_key), Some(calendar_href), Ok(base_url)) => {
                        event_dialog::CreateTarget::Caldav {
                            calendar_id: calendar.id,
                            name: calendar.name,
                            base_url,
                            username,
                            token_key,
                            calendar_href,
                        }
                    }
                    _ => event_dialog::CreateTarget::Unavailable {
                        calendar_id: calendar.id,
                        name: calendar.name,
                        error: "CalDAV calendar is missing sync metadata".to_string(),
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
    let start = target_date
        .and_time(target_time.unwrap_or_else(|| event.start.time()))
        .and_local_timezone(Local)
        .single()
        .unwrap_or(event.start);
    // An all-day span is a count of calendar days, not elapsed hours: a DST
    // transition inside the original span would otherwise pull the moved end
    // off midnight and corrupt the exclusive end date.
    let end = if event.all_day {
        let span_days = (event.end.date_naive() - event.start.date_naive())
            .num_days()
            .max(1);
        (start.date_naive() + ChronoDuration::days(span_days))
            .and_time(NaiveTime::MIN)
            .and_local_timezone(Local)
            .single()
            .unwrap_or(start + (event.end - event.start))
    } else {
        start + (event.end - event.start)
    };
    EventDraft {
        title: event.title.clone(),
        start,
        end,
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

struct CaldavAddResult {
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
        let result = (|| -> Result<CaldavAddResult, String> {
            let credentials = caldav::Credentials {
                base_url: icloud::ICLOUD_CALDAV_ROOT.to_string(),
                username: apple_id.clone(),
                password: app_password,
            };
            caldav::discover_calendars(&credentials)?;

            let token_key = icloud::credentials::token_key(&apple_id);
            let store = Store::open().map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_icloud_account(&apple_id, &apple_id, &token_key)
                .map_err(|e| e.to_string())?;
            icloud::credentials::save_app_password(&token_key, &credentials.password)
                .map_err(|e| e.to_string())?;
            let calendars_synced = caldav::sync_account(&credentials, &store, account_id)?;
            Ok(CaldavAddResult {
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
/// Fires a quiet background sync for every provider that has a connected
/// account and isn't already syncing. Shared by the launch pass and the
/// periodic re-sync timer. A disabled sync button marks a provider whose sync
/// is still in flight, so it's skipped rather than stacking a second request.
fn sync_connected_accounts(
    ui: &Rc<Ui>,
    google_sync_button: &gtk::Button,
    icloud_sync_button: &gtk::Button,
    caldav_sync_button: &gtk::Button,
) {
    if google_sync_button.is_sensitive()
        && ui
            .store
            .google_accounts()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    {
        sync_google_accounts(ui, google_sync_button, true);
    }
    if icloud_sync_button.is_sensitive()
        && ui
            .store
            .icloud_accounts()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    {
        sync_icloud_accounts(ui, icloud_sync_button, true);
    }
    if caldav_sync_button.is_sensitive()
        && ui
            .store
            .caldav_accounts()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    {
        sync_caldav_accounts(ui, caldav_sync_button, true);
    }
}

/// `quiet` suppresses the success toast (errors are always surfaced) so
/// automatic launch/periodic syncs don't nag; manual clicks pass `false`.
fn sync_google_accounts(ui: &Rc<Ui>, sync_button: &gtk::Button, quiet: bool) {
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
                    if !quiet {
                        ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                            "Synced {calendar_count} calendar(s) from {account_count} account(s)"
                        )));
                    }
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

fn sync_icloud_accounts(ui: &Rc<Ui>, sync_button: &gtk::Button, quiet: bool) {
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
                let credentials = caldav::Credentials {
                    base_url: icloud::ICLOUD_CALDAV_ROOT.to_string(),
                    username: account.provider_account_id.clone(),
                    password: app_password,
                };
                calendar_count += caldav::sync_account(&credentials, &store, account.id)?;
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
                    if !quiet {
                        ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                            "Synced {calendar_count} iCloud calendar(s) from {account_count} account(s)"
                        )));
                    }
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

fn update_caldav_sync_button(ui: &Rc<Ui>, button: &gtk::Button) {
    let account_count = ui
        .store
        .caldav_accounts()
        .map(|accounts| accounts.len())
        .unwrap_or(0);
    button.set_sensitive(true);
    button.set_tooltip_text(if account_count > 0 {
        Some("Fetch the latest events from connected CalDAV accounts")
    } else {
        Some("Fetch calendars from connected CalDAV accounts")
    });
}

fn open_caldav_account_dialog(ui: &Rc<Ui>, add_button: &gtk::Button, sync_button: &gtk::Button) {
    let dialog = adw::Dialog::builder()
        .title("Add CalDAV")
        .content_width(440)
        .build();

    let cancel_button = gtk::Button::with_label("Cancel");
    let connect_button = gtk::Button::builder()
        .label("Connect")
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_button);
    header.pack_end(&connect_button);

    let server_row = adw::EntryRow::builder().title("Server URL").build();
    let username_row = adw::EntryRow::builder().title("Username").build();
    let password_row = adw::PasswordEntryRow::builder().title("Password").build();
    let http_row = adw::SwitchRow::builder()
        .title("Allow unencrypted HTTP")
        .subtitle("Sends your password in cleartext — only for trusted local networks")
        .build();

    let group = adw::PreferencesGroup::new();
    group.add(&server_row);
    group.add(&username_row);
    group.add(&password_row);
    group.add(&http_row);

    let note = gtk::Label::new(Some(
        "Enter your provider's CalDAV address — e.g. https://caldav.fastmail.com/ \
         or your Nextcloud URL. Many providers want an app password rather than \
         your login password.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");

    let error_label = gtk::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.set_visible(false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&group);
    content.append(&note);
    content.append(&error_label);

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
        #[weak]
        error_label,
        move |_| {
            let server_url = server_row.text().trim().to_string();
            let username = username_row.text().trim().to_string();
            let password = password_row.text().to_string();
            if server_url.is_empty() || username.is_empty() || password.is_empty() {
                error_label.set_label("Server URL, username, and password are all required.");
                error_label.set_visible(true);
                return;
            }
            if !(server_url.starts_with("http://") || server_url.starts_with("https://")) {
                error_label.set_label("The server URL must start with http:// or https://.");
                error_label.set_visible(true);
                return;
            }
            let server_url = match caldav::canonical_base_url(&server_url) {
                Ok(url) => url,
                Err(message) => {
                    error_label.set_label(&message);
                    error_label.set_visible(true);
                    return;
                }
            };
            if server_url.starts_with("http://") && !http_row.is_active() {
                error_label.set_label(
                    "This server uses unencrypted HTTP, which would expose your \
                     password to the network. Use an https:// URL, or enable \
                     “Allow unencrypted HTTP” for a trusted local network.",
                );
                error_label.set_visible(true);
                return;
            }
            dialog.close();
            add_caldav_account(
                &ui,
                &add_button,
                &sync_button,
                server_url,
                username,
                password,
            );
        }
    ));

    dialog.present(Some(&ui.carousel));
}

fn add_caldav_account(
    ui: &Rc<Ui>,
    add_button: &gtk::Button,
    sync_button: &gtk::Button,
    server_url: String,
    username: String,
    password: String,
) {
    add_button.set_sensitive(false);
    add_button.set_label("Connecting…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<CaldavAddResult, String> {
            let credentials = caldav::Credentials {
                base_url: server_url.clone(),
                username: username.clone(),
                password,
            };
            // Verify the credentials and reachability before persisting.
            caldav::discover_calendars(&credentials)?;

            let token_key = icloud::credentials::caldav_token_key(&server_url, &username);
            let store = Store::open().map_err(|e| e.to_string())?;
            let display_name = format!("{username} ({})", host_label(&server_url));
            let account_id = store
                .upsert_caldav_account(&username, &server_url, &display_name, &token_key)
                .map_err(|e| e.to_string())?;
            icloud::credentials::save_app_password(&token_key, &credentials.password)
                .map_err(|e| e.to_string())?;
            let calendars_synced = caldav::sync_account(&credentials, &store, account_id)?;
            Ok(CaldavAddResult {
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
                    add_button.set_label("Add CalDAV");
                    add_button.set_sensitive(true);
                    update_caldav_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "CalDAV connect failed: {}",
                            first_line(&error)
                        ))));
                    add_button.set_label("Add CalDAV");
                    add_button.set_sensitive(true);
                    update_caldav_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    add_button.set_label("Add CalDAV");
                    add_button.set_sensitive(true);
                    update_caldav_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}

fn sync_caldav_accounts(ui: &Rc<Ui>, sync_button: &gtk::Button, quiet: bool) {
    sync_button.set_sensitive(false);
    sync_button.set_label("Syncing…");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<(usize, usize), String> {
            let store = Store::open().map_err(|e| e.to_string())?;
            let accounts = store.caldav_accounts().map_err(|e| e.to_string())?;
            if accounts.is_empty() {
                return Err("No CalDAV accounts connected. Use Add CalDAV first.".to_string());
            }

            let account_count = accounts.len();
            let mut calendar_count = 0;
            for account in accounts {
                let Some(base_url) = account.server_url.clone() else {
                    return Err(format!(
                        "{} is missing its server address; remove and re-add it.",
                        account.display_name
                    ));
                };
                let password = icloud::credentials::app_password(&account.token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!("missing saved password for {}", account.display_name)
                    })?;
                let credentials = caldav::Credentials {
                    base_url,
                    username: account.provider_account_id.clone(),
                    password,
                };
                calendar_count += caldav::sync_account(&credentials, &store, account.id)?;
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
                    if !quiet {
                        ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                            "Synced {calendar_count} CalDAV calendar(s) from {account_count} account(s)"
                        )));
                    }
                    sync_button.set_label("Sync CalDAV");
                    update_caldav_sync_button(&ui, &sync_button);
                    ui.reset_calendar_sidebar();
                    ui.reset();
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.toast_overlay
                        .add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                            "CalDAV sync failed: {}",
                            first_line(&error)
                        ))));
                    sync_button.set_label("Sync CalDAV");
                    update_caldav_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    sync_button.set_label("Sync CalDAV");
                    sync_button.set_sensitive(true);
                    update_caldav_sync_button(&ui, &sync_button);
                    glib::ControlFlow::Break
                }
            }
        ),
    );
}

/// Host portion of a server URL, for a compact account label; falls back to
/// the raw string if it doesn't parse.
fn host_label(server_url: &str) -> String {
    url::Url::parse(server_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| server_url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local_midnight(year: i32, month: u32, day: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("unambiguous local midnight")
    }

    fn test_event(start: DateTime<Local>, end: DateTime<Local>, all_day: bool) -> Event {
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Test".to_string(),
            calendar_color: "#3584e4".to_string(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Trip".to_string(),
            start,
            end,
            all_day,
            location: None,
            notes: None,
            google_event_id: None,
            icloud_event_id: None,
            account_server_url: None,
        }
    }

    #[test]
    fn moved_all_day_draft_keeps_its_calendar_day_span() {
        // March 7–9, 2026 spans the US spring-forward transition, so in a DST
        // timezone the elapsed duration is not a whole number of days.
        let event = test_event(local_midnight(2026, 3, 7), local_midnight(2026, 3, 9), true);

        let target = NaiveDate::from_ymd_opt(2026, 3, 16).unwrap();
        let draft = moved_draft(&event, target, None);

        assert_eq!(draft.start.date_naive(), target);
        assert_eq!(draft.start.time(), NaiveTime::MIN);
        assert_eq!(
            draft.end.date_naive(),
            NaiveDate::from_ymd_opt(2026, 3, 18).unwrap()
        );
        assert_eq!(draft.end.time(), NaiveTime::MIN);
    }

    #[test]
    fn moved_timed_draft_keeps_its_elapsed_duration() {
        let start = Local
            .with_ymd_and_hms(2026, 7, 6, 9, 30, 0)
            .single()
            .unwrap();
        let event = test_event(start, start + ChronoDuration::minutes(45), false);

        let target = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
        let draft = moved_draft(&event, target, NaiveTime::from_hms_opt(14, 0, 0));

        assert_eq!(draft.start.date_naive(), target);
        assert_eq!(draft.end - draft.start, ChronoDuration::minutes(45));
    }
}
