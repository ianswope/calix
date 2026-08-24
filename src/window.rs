use crate::caldav;
use crate::calendar_dialog;
use crate::config::Config;
use crate::date_util::{
    day_bounds, month_grid_bounds, month_start, shift_days, shift_months, shift_weeks, shift_years,
    week_bounds, week_dates, week_start, year_bounds, year_start,
};
use crate::event_dialog;
use crate::event_popover;
use crate::google;
use crate::icloud;
use crate::provider::{self, Provider};
use crate::search;
use crate::store::{self, Event, EventDraft, Store};
use crate::sync::{self, SyncOutcome};
use crate::undo;
use crate::views::{drag::DragKind, month_view, week_view, year_view};
use adw::prelude::*;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, NaiveTime};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// A command reachable from the keyboard.
///
/// Kept separate from the widgets that perform it so the mapping is a pure
/// function: which key means what is the part worth testing, and it needs no
/// display to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyCommand {
    Undo,
    Redo,
    Today,
    Previous,
    Next,
    ViewYear,
    ViewMonth,
    ViewWeek,
    ViewDay,
    NewEvent,
    Search,
}

/// Maps a keypress to the command it triggers, or `None` to let it through.
///
/// Every binding requires Ctrl. Plain keys are deliberately unbound: this
/// controller sits on the window, so an unmodified letter reaching it would be
/// a letter the user was typing into an event title. Requiring a modifier is
/// what keeps a global shortcut from eating text entry.
fn key_command(key: gdk::Key, state: gdk::ModifierType) -> Option<KeyCommand> {
    // Caps Lock rides along on ordinary presses and says nothing about
    // intent, so it's ignored. Alt, Super, Hyper and Meta do change intent, so
    // a chord carrying one of them belongs to some other binding, not ours.
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let other = state.intersects(
        gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::HYPER_MASK
            | gdk::ModifierType::META_MASK,
    );
    if !ctrl || other {
        return None;
    }
    match key {
        // Shift is meaningful for exactly this pair, because Ctrl+Shift+Z is
        // the redo chord everyone arrives with. It is read off the modifier
        // rather than the uppercase keyval so a stuck Lock key can't reverse
        // the command — see `caps_lock_does_not_turn_an_undo_into_a_redo`.
        gdk::Key::z | gdk::Key::Z => {
            if state.contains(gdk::ModifierType::SHIFT_MASK) {
                Some(KeyCommand::Redo)
            } else {
                Some(KeyCommand::Undo)
            }
        }
        gdk::Key::y | gdk::Key::Y => Some(KeyCommand::Redo),
        // Numbered left-to-right as the header's toggles read, so the digit
        // matches what the eye sees. Apple numbers by its View menu instead,
        // which would put Day on Ctrl+1 while Month sits leftmost on screen.
        gdk::Key::_1 | gdk::Key::KP_1 => Some(KeyCommand::ViewYear),
        gdk::Key::_2 | gdk::Key::KP_2 => Some(KeyCommand::ViewMonth),
        gdk::Key::_3 | gdk::Key::KP_3 => Some(KeyCommand::ViewWeek),
        gdk::Key::_4 | gdk::Key::KP_4 => Some(KeyCommand::ViewDay),
        gdk::Key::t | gdk::Key::T => Some(KeyCommand::Today),
        gdk::Key::n | gdk::Key::N => Some(KeyCommand::NewEvent),
        gdk::Key::f | gdk::Key::F => Some(KeyCommand::Search),
        gdk::Key::Left | gdk::Key::KP_Left => Some(KeyCommand::Previous),
        gdk::Key::Right | gdk::Key::KP_Right => Some(KeyCommand::Next),
        _ => None,
    }
}

use crate::views::CreateFn;
use crate::views::EditFn;
type MoveFn = Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>;
/// Work parked until a rebuild verifiably centers the carousel, run once.
type SettledFn = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// What account work is in flight: a sync per provider, and the one
/// interactive sign-in that has no dialog of its own to disable.
///
/// This used to be read off three `gtk::Button`s that were built but never
/// added to the sidebar, with `sensitive` standing in for "idle". Nothing
/// owned them, so they were finalized as soon as `build_window` returned and
/// every `#[weak]` upgrade afterwards failed silently: no sync at launch, none
/// on the timer, none after resume, and a Connect/Refresh/Manage row that did
/// nothing. Application state doesn't belong in a widget that isn't on screen
/// — least of all one nothing is holding.
#[derive(Default)]
struct AccountActivity {
    syncing: [Cell<bool>; provider::ALL.len()],
    signing_in: Cell<bool>,
}

impl AccountActivity {
    fn slot(&self, provider: Provider) -> &Cell<bool> {
        let index = provider::ALL
            .iter()
            .position(|candidate| *candidate == provider)
            .expect("every Provider is one of provider::ALL");
        &self.syncing[index]
    }

    /// Claims `provider` for a new sync. `false` means one is already running
    /// and the caller must not start a second.
    fn start_sync(&self, provider: Provider) -> bool {
        !self.slot(provider).replace(true)
    }

    fn finish_sync(&self, provider: Provider) {
        self.slot(provider).set(false);
    }

    fn is_syncing(&self, provider: Provider) -> bool {
        self.slot(provider).get()
    }

    /// Whether any provider is mid-sync — what the one Refresh control shows.
    fn any_sync_in_flight(&self) -> bool {
        self.syncing.iter().any(Cell::get)
    }

    /// Claims the interactive sign-in. `false` means one is already open.
    ///
    /// Only the Google flow needs this: it hands off to a browser with no
    /// dialog left on screen, so without a guard a second click starts a
    /// second OAuth round-trip and a second redirect listener.
    fn start_sign_in(&self) -> bool {
        !self.signing_in.replace(true)
    }

    fn finish_sign_in(&self) {
        self.signing_in.set(false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Year,
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
            Some("year") => ViewMode::Year,
            _ => ViewMode::Month,
        }
    }

    fn as_setting(self) -> &'static str {
        match self {
            ViewMode::Year => "year",
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

/// Index of the "current" page among the three (prev, current, next) pages a
/// rebuild appends to the carousel.
const MIDDLE_PAGE: f64 = 1.0;

/// How close `AdwCarousel:position` must sit to a page index to count as
/// parked on it. The property is a float that animates, so an exact compare
/// would never match.
const POSITION_EPSILON: f64 = 1e-6;

/// Tracks which rebuild owns the carousel and whether that rebuild has been
/// confirmed to sit on its middle page.
///
/// A single "rebuilding" boolean can't do this job. It records *that* a
/// rebuild is in flight but not *which*, so whichever settle loop happened to
/// finish last cleared the guard for everybody — including a loop belonging to
/// a rebuild that had already been superseded. With the guard down and the
/// carousel still parked somewhere else, the next `page-changed` read as a
/// user swipe and moved the period a second time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CarouselSync {
    /// Bumped by every rebuild. The current value names the only rebuild
    /// whose settle loop is still allowed to speak for the carousel.
    generation: u64,
    /// The generation whose recentering was actually confirmed on screen.
    settled: Option<u64>,
}

impl CarouselSync {
    /// Claims the carousel for a new rebuild, invalidating any in-flight one,
    /// and returns the generation that rebuild should identify itself by.
    fn begin_rebuild(&mut self) -> u64 {
        self.generation += 1;
        self.settled = None;
        self.generation
    }

    /// Records that `generation` verified itself parked on the middle page. A
    /// superseded generation is ignored, so a late loop can never vouch for a
    /// newer rebuild it knows nothing about.
    fn mark_settled(&mut self, generation: u64) {
        if self.owns(generation) {
            self.settled = Some(generation);
        }
    }

    /// Whether a settle loop running as `generation` is still authoritative.
    fn owns(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Whether the carousel is verifiably parked where the model says it is.
    /// This is the only state in which `page-changed` means "the user
    /// swiped" — at any other moment it's fallout from our own scrolling,
    /// or from a rebuild that hasn't landed yet.
    fn is_settled(&self) -> bool {
        self.settled == Some(self.generation)
    }
}

/// Everything one frame of the recentering loop can observe.
#[derive(Debug, Clone, Copy)]
struct SettleFrame {
    /// This loop's generation is still the current one.
    owns_carousel: bool,
    /// The page this loop is centering on is still in the carousel.
    page_attached: bool,
    /// Carousel width. Zero means no allocation yet, or a frame clock stalled
    /// while the screen is blanked/locked, and `scroll_to` would silently
    /// no-op because it resolves its jump as `position * width`.
    width: i32,
    /// `AdwCarousel:position`, in pages.
    position: f64,
    /// This loop has already issued a `scroll_to` of its own, with real
    /// geometry, for its own rebuild.
    scrolled: bool,
}

/// What one frame-clock step of recentering a rebuilt carousel should do.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SettleAction {
    /// Superseded by a newer rebuild — stop without touching shared state.
    Abandon,
    /// No real geometry yet — a scroll issued now would silently stay on
    /// page 0, so keep waiting for frames.
    Wait,
    /// Not verifiably centered by *this* rebuild yet — issue a scroll and
    /// check again next frame.
    Scroll,
    /// Centered by this rebuild and parked there — `page-changed` can be
    /// trusted again.
    Done,
}

fn settle_action(frame: SettleFrame) -> SettleAction {
    if !frame.owns_carousel || !frame.page_attached {
        SettleAction::Abandon
    } else if frame.width <= 0 {
        SettleAction::Wait
    } else if !frame.scrolled || (frame.position - MIDDLE_PAGE).abs() > POSITION_EPSILON {
        // `scrolled` is what makes a position reading trustworthy. A rebuild
        // that inherits a stale 1.0 from the rebuild it replaced would
        // otherwise declare itself centered without ever having scrolled,
        // leaving whatever page was already showing on screen.
        SettleAction::Scroll
    } else {
        SettleAction::Done
    }
}

/// Which end of the carousel a one-period step swaps out.
///
/// A swipe leaves the page the user is now looking at attached at the near end.
/// Only the far page — two periods stale, and offscreen — may be replaced;
/// destroying and rebuilding the visible page mid-animation is what makes the
/// whole view flicker.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct RecyclePlan {
    /// Remove the carousel's first child rather than its last.
    drop_first: bool,
    /// Insert the replacement at the front rather than appending it.
    insert_first: bool,
}

fn recycle_plan(delta: i32) -> RecyclePlan {
    if delta > 0 {
        RecyclePlan {
            drop_first: true,
            insert_first: false,
        }
    } else {
        RecyclePlan {
            drop_first: false,
            insert_first: true,
        }
    }
}

/// Which way a landed `page-changed` index moves the period, if at all.
/// Index 1 is the middle page — where every rebuild parks the carousel — so
/// landing there is our own centering, never a swipe.
fn swipe_delta(index: u32) -> Option<i32> {
    match index {
        0 => Some(-1),
        2 => Some(1),
        _ => None,
    }
}

struct State {
    view_mode: ViewMode,
    current_date: NaiveDate,
    hour_row_height: i32,
}

impl State {
    fn period_anchor(&self) -> NaiveDate {
        match self.view_mode {
            ViewMode::Year => year_start(self.current_date),
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
            ViewMode::Year => shift_years(date, delta),
            ViewMode::Month => shift_months(date, delta),
            ViewMode::Week => shift_weeks(date, delta),
            ViewMode::Day => shift_days(date, delta),
        }
    }

    fn title(&self) -> String {
        match self.view_mode {
            ViewMode::Year => self.period_anchor().format("%Y").to_string(),
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
    /// Holds the sidebar's mini month. It follows `current_date` rather than
    /// carrying an anchor of its own, so there is no second notion of "the
    /// month being looked at" that could drift from the main view's.
    mini_month: gtk::Box,
    title_label: gtk::Label,
    toast_overlay: adw::ToastOverlay,
    state: Rc<RefCell<State>>,
    store: Rc<Store>,
    config: Rc<RefCell<Config>>,
    // The calendar date the display is currently anchored to. A periodic clock
    // tick compares it against the real date so a rollover (left open
    // overnight, or crossed while the machine was suspended) can be noticed and
    // the "today" highlighting re-anchored.
    today: Rc<Cell<NaiveDate>>,
    // Which rebuild owns the carousel, and whether it has landed. Everything
    // that must not act on a half-rebuilt carousel consults this.
    sync: Rc<Cell<CarouselSync>>,
    // Run once by the next rebuild that verifiably lands. Startup uses it to
    // connect the interactive handlers on the same clock that centers the
    // first page, instead of guessing at a delay.
    on_settled: SettledFn,
    // Set when a zoom updated only the visible page in place, leaving the
    // offscreen neighbor pages at the old height. Navigation then preserves
    // the hour the user was looking at across the rebuild.
    zoom_dirty: Rc<Cell<bool>>,
    // What account work is in flight. Owned by the `Ui` that every closure
    // already holds, so a timer can't find it gone the way it found the old
    // unparented sync buttons gone.
    activity: Rc<AccountActivity>,
    // The one visible account-refresh control. On the `Ui` because the sync
    // plumbing reports in-flight state through it, and because a widget the
    // `Ui` owns is a widget that is still alive when a callback runs.
    refresh_accounts_button: gtk::Button,
    // What the user has done and can take back, for this window's lifetime.
    // Not persisted: an undo describes a write against a row as it stood, and
    // after a restart the provider has usually moved on.
    history: Rc<RefCell<undo::History>>,
    // A remote undo is a round trip, and the change stays on the stack until it
    // lands. Without this a second Ctrl+Z during that window would try the same
    // change again and double-write it.
    history_busy: Rc<Cell<bool>>,
}

/// Which way through the history one keypress goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryStep {
    Undo,
    Redo,
}

impl HistoryStep {
    fn nothing_to_do(self) -> &'static str {
        match self {
            Self::Undo => "Nothing to undo",
            Self::Redo => "Nothing to redo",
        }
    }

    /// What to say when the row no longer holds what the change wrote.
    fn changed_since(self) -> &'static str {
        match self {
            Self::Undo => "That event has changed since — there's nothing to take back",
            Self::Redo => "That event has changed since — that change can't be put back",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Undo => "undo that",
            Self::Redo => "redo that",
        }
    }
}

impl Ui {
    fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    /// Takes back the last change the user made, or puts it back again.
    ///
    /// The decisions all live in [`undo`]: this reads the row as it stands,
    /// asks for the write that reverses the change, and carries it out — on the
    /// provider first when the event lives on one, because a local-only undo
    /// would be quietly reverted by the next sync.
    fn step_history(self: &Rc<Self>, step: HistoryStep) {
        if self.history_busy.get() {
            return;
        }
        let change = {
            let history = self.history.borrow();
            match step {
                HistoryStep::Undo => history.peek_undo().cloned(),
                HistoryStep::Redo => history.peek_redo().cloned(),
            }
        };
        let Some(change) = change else {
            self.toast(step.nothing_to_do());
            return;
        };
        let current = change
            .id
            .and_then(|id| self.store.event_by_id(id).ok().flatten());
        let current_draft = current.as_ref().map(store::Event::draft);
        let write = match step {
            HistoryStep::Undo => change.undo(current_draft.as_ref()),
            HistoryStep::Redo => change.redo(current_draft.as_ref()),
        };
        let Ok(write) = write else {
            self.discard(step);
            self.toast(step.changed_since());
            return;
        };
        if undo::needs_a_remote_create(&write, self.calendar_is_local(change.calendar_id)) {
            self.discard(step);
            self.toast("Calix can't put a synced event back on its calendar yet");
            return;
        }
        self.apply_history_write(step, write, current);
    }

    /// Drops a change that can no longer be applied, so the next keypress
    /// reaches the one behind it instead of retrying this one forever.
    fn discard(self: &Rc<Self>, step: HistoryStep) {
        let mut history = self.history.borrow_mut();
        match step {
            HistoryStep::Undo => history.discard_undo(),
            HistoryStep::Redo => history.discard_redo(),
        }
    }

    fn calendar_is_local(&self, calendar_id: i64) -> bool {
        self.store
            .local_calendars()
            .is_ok_and(|calendars| calendars.iter().any(|calendar| calendar.id == calendar_id))
    }

    /// Sends the write to the provider when there is one, then to SQLite.
    fn apply_history_write(
        self: &Rc<Self>,
        step: HistoryStep,
        write: undo::Write,
        current: Option<Event>,
    ) {
        let remote = current
            .as_ref()
            .and_then(|event| remote_event_handler(self, event));
        let remote = match remote {
            // The account is in no state to be written to. Leave the change on
            // the stack: reconnecting makes it applicable again.
            Some(event_dialog::RemoteEvent::Unavailable(error)) => {
                self.toast(&error);
                return;
            }
            Some(remote) => remote,
            None => {
                self.commit_history_write(step, &write);
                return;
            }
        };
        let call: Box<dyn FnOnce() -> Result<(), String> + Send> = match &write {
            undo::Write::Update { draft, .. } => {
                let draft = draft.clone();
                Box::new(move || remote.update(&draft))
            }
            undo::Write::Delete { .. } => Box::new(move || remote.delete()),
            // Refused before we got here — restoring onto a provider would
            // need a remote create.
            undo::Write::Insert { .. } => return,
        };
        self.history_busy.set(true);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(call());
        });
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
            Ok(Ok(())) => {
                ui.history_busy.set(false);
                ui.commit_history_write(step, &write);
                glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                ui.history_busy.set(false);
                ui.toast(&format!("Couldn't {}: {error}", step.verb()));
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                ui.history_busy.set(false);
                ui.toast("That change stopped unexpectedly");
                glib::ControlFlow::Break
            }
        });
    }

    /// Writes to SQLite and moves the change to the other stack, carrying the
    /// id the row has now — a restored row comes back with a new one.
    fn commit_history_write(self: &Rc<Self>, step: HistoryStep, write: &undo::Write) {
        match undo::apply(&self.store, write) {
            Ok(id) => {
                let mut history = self.history.borrow_mut();
                match step {
                    HistoryStep::Undo => history.commit_undo(id),
                    HistoryStep::Redo => history.commit_redo(id),
                }
                drop(history);
                self.reset();
            }
            Err(error) => self.toast(&format!("Couldn't {}: {error}", step.verb())),
        }
    }

    /// Remembers a change the user just made, so it can be taken back.
    fn record(&self, change: undo::Change) {
        self.history.borrow_mut().record(change);
    }
}

impl Ui {
    /// Redraws after the event dialog saved something, and goes back to the
    /// provider first when the local cache can't be right on its own — see
    /// [`event_dialog::Saved`].
    fn apply_saved(self: &Rc<Self>, saved: event_dialog::Saved) {
        self.reset();
        if saved == event_dialog::Saved::StaleUntilSync {
            self.request_background_sync();
        }
    }

    /// Asks for a quiet background sync of every connected provider.
    fn request_background_sync(self: &Rc<Self>) {
        sync_connected_accounts(self);
    }

    /// Clears the carousel and rebuilds it with prev/current/next pages
    /// centered on the selected date, landing on the usual "now" scroll spot.
    fn reset(self: &Rc<Self>) {
        self.reset_with(week_view::InitialScroll::NowOrMorning);
    }

    /// `reset`, but landing the timed grid at `scroll` — used to keep the same
    /// time in view when a full rebuild happens for reasons other than
    /// navigation (e.g. the first swipe after an in-place zoom).
    fn reset_with(self: &Rc<Self>, scroll: week_view::InitialScroll) {
        let generation = {
            let mut sync = self.sync.get();
            let generation = sync.begin_rebuild();
            self.sync.set(sync);
            generation
        };
        // A full rebuild makes every page current again.
        self.zoom_dirty.set(false);

        // The mini month follows the same date, so it is rebuilt on the same
        // beat rather than left showing the month the user just navigated away
        // from.
        self.reset_mini_month();

        let mut child = self.carousel.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.carousel.remove(&widget);
            child = next;
        }

        let state = self.state.borrow();
        let view_mode = state.view_mode;
        self.carousel.set_orientation(match view_mode {
            ViewMode::Year | ViewMode::Month => gtk::Orientation::Vertical,
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

        // Center synchronously first. When the carousel already has geometry
        // — every rebuild after startup — this lands before GTK paints, so a
        // swipe doesn't flash the neighbor page that appending leaves at
        // position 0. It silently no-ops when width is still 0, which is what
        // the frame-clock loop below is for.
        self.carousel.scroll_to(&current_page, false);

        self.confirm_centered(generation, current_page);
    }

    /// Retries the scroll onto `current_page` each frame until the carousel is
    /// verifiably parked there, then marks `generation` settled.
    ///
    /// On the frame clock, never on a wall-clock delay: while the screen is
    /// blanked, the session is locked, or the machine is mid-resume, the
    /// compositor sends no frames, layout never runs, and `scroll_to` silently
    /// stays on page 0 — the previous period. A timer fires anyway and would
    /// clear the guard over a carousel sitting on the wrong page; a tick
    /// callback only runs once frames flow again.
    fn confirm_centered(self: &Rc<Self>, generation: u64, current_page: gtk::Widget) {
        let ui = self.clone();
        // Tick callbacks are `Fn`, so this loop's own progress lives in a Cell.
        let scrolled = Cell::new(false);
        self.carousel.add_tick_callback(move |carousel, _clock| {
            let frame = SettleFrame {
                owns_carousel: ui.sync.get().owns(generation),
                page_attached: current_page.parent().as_ref() == Some(carousel.upcast_ref()),
                width: carousel.width(),
                position: carousel.position(),
                scrolled: scrolled.get(),
            };
            match settle_action(frame) {
                SettleAction::Abandon => glib::ControlFlow::Break,
                SettleAction::Wait => glib::ControlFlow::Continue,
                SettleAction::Scroll => {
                    carousel.scroll_to(&current_page, false);
                    scrolled.set(true);
                    glib::ControlFlow::Continue
                }
                SettleAction::Done => {
                    ui.settled(generation);
                    glib::ControlFlow::Break
                }
            }
        });
    }

    /// Marks `generation`'s rebuild as landed and runs the work that was
    /// waiting on a carousel whose position can be trusted.
    fn settled(self: &Rc<Self>, generation: u64) {
        let mut sync = self.sync.get();
        sync.mark_settled(generation);
        self.sync.set(sync);

        if let Some(action) = self.on_settled.borrow_mut().take() {
            action();
        }
        // A date rollover noticed while this rebuild was pending was deferred
        // (tick_clock skips rollovers mid-rebuild); apply it now instead of
        // waiting out the 30s timer.
        self.tick_clock();
    }

    /// Claims the carousel for an imminent rebuild, so `page-changed` stops
    /// being trusted before any delay this navigation needs to wait out.
    fn begin_rebuild(&self) -> u64 {
        let mut sync = self.sync.get();
        let generation = sync.begin_rebuild();
        self.sync.set(sync);
        generation
    }

    /// Moves the display by `delta` periods with a full rebuild of all three
    /// pages. Used where there's no page worth keeping — arrow buttons, view
    /// mode changes, a date rollover.
    fn navigate(self: &Rc<Self>, delta: i32) {
        // A pinch re-zoomed only the visible page, so the rebuild would
        // otherwise drop the user back at "now". Read the hour they're
        // looking at before the pages go away.
        let scroll = if self.zoom_dirty.get() {
            self.visible_scroll_hours()
                .map(week_view::InitialScroll::AtHour)
                .unwrap_or(week_view::InitialScroll::NowOrMorning)
        } else {
            week_view::InitialScroll::NowOrMorning
        };

        let mut state = self.state.borrow_mut();
        state.current_date = state.shift(delta);
        drop(state);

        self.reset_with(scroll);
    }

    /// Completes a swipe: the user has already animated onto a neighbor page,
    /// so adopt it as the current one and replace only the far, offscreen page.
    ///
    /// This is why `reset_with` isn't used here. A full rebuild destroys and
    /// re-creates the page the user is looking at, in the middle of the swipe
    /// animation, and the whole view flickers. Correctness doesn't depend on
    /// having one rebuild path — it comes from `CarouselSync` and the settle
    /// loop, which this path uses exactly as `reset_with` does.
    fn advance(self: &Rc<Self>, delta: i32) {
        let generation = self.begin_rebuild();

        let mut state = self.state.borrow_mut();
        state.current_date = state.shift(delta);
        drop(state);

        // A zoom left the neighbor pages at the old hour height, so the page
        // just swiped to is stale and can't be adopted. Rebuild all three,
        // keeping the time the user was looking at; `reset_with` supersedes
        // this generation, which its own settle loop then owns.
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
        let replacement_date = state.shift_from(state.current_date, delta);
        let title = state.title();
        drop(state);

        let replacement = self.build_page(
            view_mode,
            replacement_date,
            week_view::InitialScroll::NowOrMorning,
        );
        let plan = recycle_plan(delta);
        let stale = if plan.drop_first {
            self.carousel.first_child()
        } else {
            self.carousel.last_child()
        };
        if let Some(stale) = stale {
            self.carousel.remove(&stale);
        }
        if plan.insert_first {
            self.carousel.insert(&replacement, 0);
        } else {
            self.carousel.append(&replacement);
        }
        self.title_label.set_label(&title);

        let Some(current_page) = self
            .carousel
            .first_child()
            .and_then(|page| page.next_sibling())
        else {
            // Nothing to center on; leave the carousel unsettled rather than
            // vouch for a page that doesn't exist, and let the next full
            // rebuild recover.
            return;
        };
        // Inserting the new previous page briefly puts it at position zero.
        // Recenter before GTK can paint it, or the swipe shows the wrong
        // period for a frame.
        self.carousel.scroll_to(&current_page, false);
        self.confirm_centered(generation, current_page);
    }

    /// Runs on a periodic timer to keep the display anchored to real time.
    /// Slides the "now" line to the current time, and when the calendar date
    /// has rolled over re-anchors "today": the highlighting always follows the
    /// real day, and if the user is still parked on today the visible page
    /// follows too. Also invoked directly on resume from suspend (see the
    /// logind listener in `build_ui`), which is what recovers a day boundary
    /// crossed while asleep — GLib's monotonic timer is frozen during suspend,
    /// so it can't be relied on to fire at wake.
    fn tick_clock(self: &Rc<Self>) {
        let now_date = Local::now().date_naive();
        let previous = self.today.get();

        // Don't disturb an in-progress swipe/rebuild; the next tick retries
        // (with `today` still unchanged, so the rollover isn't lost), and the
        // landing rebuild calls back in here as soon as it settles.
        if now_date != previous && self.sync.get().is_settled() {
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

    /// Rebuilds the sidebar's mini month from the current date.
    ///
    /// Its arrows move the main view by a month rather than paging the
    /// thumbnail alone. One date is being looked at, not two, and a mini month
    /// showing March while the grid shows August is a bug users have to hold in
    /// their head.
    fn reset_mini_month(self: &Rc<Self>) {
        let mut child = self.mini_month.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.mini_month.remove(&widget);
            child = next;
        }

        let anchor = self.state.borrow().current_date;
        let (range_start, range_end) = month_grid_bounds(anchor);
        let events = self
            .store
            .events_between(store::day_start(range_start), store::day_start(range_end))
            .unwrap_or_default();

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        for button in [&prev, &next] {
            button.add_css_class("flat");
        }
        let title = gtk::Label::new(Some(&anchor.format("%B %Y").to_string()));
        title.add_css_class("heading");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        header.append(&title);
        header.append(&prev);
        header.append(&next);

        for (button, delta) in [(&prev, -1), (&next, 1)] {
            let ui = self.clone();
            button.connect_clicked(move |_| {
                let shifted = shift_months(ui.state.borrow().current_date, delta);
                ui.state.borrow_mut().current_date = shifted;
                ui.reset();
            });
        }

        let ui = self.clone();
        let thumbnail = year_view::month_thumbnail(
            month_start(anchor),
            &events,
            self.today.get(),
            Rc::new(move |picked: NaiveDate| {
                // Keeps the current view mode: the mini month answers "take me
                // to this date", not "show me a day".
                ui.state.borrow_mut().current_date = picked;
                ui.reset();
            }),
        );

        self.mini_month.append(&header);
        self.mini_month.append(&thumbnail);
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
            Rc::new(
                move |start: DateTime<Local>, end: Option<DateTime<Local>>| {
                    let ui_for_saved = ui.clone();
                    let ui_for_change = ui.clone();
                    event_dialog::open(
                        &ui.carousel,
                        ui.store.clone(),
                        create_targets(&ui),
                        None,
                        start,
                        end,
                        move |saved| ui_for_saved.apply_saved(saved),
                        move |change| ui_for_change.record(change),
                        None,
                    );
                },
            )
        };
        // Clicking an event shows the inspector; the dialog is what its Edit
        // button opens. Glancing at when something is — by far the commoner
        // reason to click — no longer costs a modal to dismiss.
        let open_dialog: Rc<dyn Fn(Event)> = {
            let ui = self.clone();
            Rc::new(move |event: Event| {
                // Local recurring events render as many occurrences that share
                // the master's id; editing any of them edits the series, so open
                // the stored master (its real start) rather than the clicked
                // occurrence, which would otherwise re-anchor the whole series.
                let event = if event.recurrence.is_some() {
                    ui.store
                        .event_by_id(event.id)
                        .ok()
                        .flatten()
                        .unwrap_or(event)
                } else {
                    event
                };
                let start = event.start;
                let ui_for_saved = ui.clone();
                let ui_for_change = ui.clone();
                let remote_event = remote_event_handler(&ui, &event);
                event_dialog::open(
                    &ui.carousel,
                    ui.store.clone(),
                    Vec::new(),
                    Some(event),
                    start,
                    None,
                    move |saved| ui_for_saved.apply_saved(saved),
                    move |change| ui_for_change.record(change),
                    remote_event,
                );
            })
        };
        let on_edit: EditFn = {
            let open_dialog = open_dialog.clone();
            let ui = self.clone();
            Rc::new(move |event: Event, anchor: gtk::Widget| {
                let remote = remote_event_handler(&ui, &event);
                let ui_for_changed = ui.clone();
                event_popover::open(
                    &anchor,
                    &event,
                    open_dialog.clone(),
                    ui.store.clone(),
                    remote,
                    Rc::new(move || ui_for_changed.reset()),
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
            ViewMode::Year => year_bounds(date),
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
            ViewMode::Year => {
                // Picking a day drops into Day view on it, which is the only
                // reason to click a date at this zoom.
                let ui = self.clone();
                year_view::build(
                    date,
                    &events,
                    Rc::new(move |picked: NaiveDate| {
                        ui.state.borrow_mut().current_date = picked;
                        set_view_mode(&ui, ViewMode::Day);
                        ui.reset();
                    }),
                )
            }
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

thread_local! {
    /// The window that's up, so a forwarded `calix <date>` can move it. Only
    /// ever read back through `open`, which drops it if the window it belongs
    /// to has since gone.
    static LIVE_UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Shows Calix on `date`, or wherever it already was when no date was asked
/// for. Every activation lands here: the first builds the window, and a second
/// `calix` invocation moves the one on screen instead of opening another.
pub fn open(app: &adw::Application, date: Option<NaiveDate>) {
    let live = LIVE_UI.with(|live| live.borrow().clone());
    match live.filter(|_| app.active_window().is_some()) {
        Some(ui) => {
            if let Some(date) = date {
                ui.state.borrow_mut().current_date = date;
                ui.reset();
            }
            if let Some(window) = app.active_window() {
                window.present();
            }
        }
        None => {
            LIVE_UI.with(|live| live.replace(None));
            build(app, date, true);
        }
    }
}

pub fn start_background(app: &adw::Application) {
    if LIVE_UI.with(|live| live.borrow().is_none()) {
        build(app, None, false);
    }
}

fn build(app: &adw::Application, date: Option<NaiveDate>, show_window: bool) {
    // Register the keyring store on the main thread before any sync worker
    // spawns, so the concurrent launch/resync threads don't race its lazy
    // initialization. See `icloud::credentials::prime_keyring_store`.
    icloud::credentials::prime_keyring_store();

    let store = match Store::open() {
        Ok(store) => Rc::new(store),
        Err(error) => {
            show_startup_error(app, &error.to_string(), date);
            return;
        }
    };
    let initial_view_mode =
        ViewMode::from_setting(store.setting(ViewMode::SETTING_KEY).unwrap_or_default());
    let initial_hour_row_height = load_hour_row_height(&store);
    let state = Rc::new(RefCell::new(State {
        view_mode: initial_view_mode,
        current_date: date.unwrap_or_else(|| Local::now().date_naive()),
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
    let mini_month = gtk::Box::new(gtk::Orientation::Vertical, 0);
    mini_month.add_css_class("mini-month");

    let calendar_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_list.set_hexpand(true);
    calendar_list.set_vexpand(true);
    let refresh_accounts_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    let title_label = gtk::Label::builder().css_classes(["title"]).build();
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title_label.set_width_chars(12);
    title_label.set_max_width_chars(28);

    let ui = Rc::new(Ui {
        carousel: carousel.clone(),
        calendar_list: calendar_list.clone(),
        mini_month: mini_month.clone(),
        title_label,
        toast_overlay: adw::ToastOverlay::new(),
        state,
        store,
        config: Rc::new(RefCell::new(Config::load())),
        today: Rc::new(Cell::new(Local::now().date_naive())),
        // Starts unsettled, so nothing treats the carousel as authoritative
        // before the first rebuild has centered it.
        sync: Rc::new(Cell::new(CarouselSync::default())),
        on_settled: Rc::new(RefCell::new(None)),
        zoom_dirty: Rc::new(Cell::new(false)),
        activity: Rc::new(AccountActivity::default()),
        refresh_accounts_button: refresh_accounts_button.clone(),
        history: Rc::new(RefCell::new(undo::History::default())),
        history_busy: Rc::new(Cell::new(false)),
    });

    LIVE_UI.with(|live| live.replace(Some(ui.clone())));

    // Keep the display anchored to real time: slide the "now" line and, on a
    // date rollover, re-anchor "today". A half-minute cadence keeps the line
    // reasonably fresh while the app is awake. This timer is frozen during
    // suspend (GLib's monotonic clock stops), so recovery from an overnight
    // sleep comes from the logind resume listener below, not from this tick.
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

    // Surface event alerts as desktop notifications. Each tick checks the
    // window since the previous one — contiguous half-open windows, so an
    // alert fires exactly once even when suspend/resume delays a tick (it
    // then fires late rather than silently dropping). The two-day query
    // horizon comfortably covers the longest lead time, one day.
    let notify_app = app.clone();
    let last_alert_check = Cell::new(Local::now());
    glib::timeout_add_seconds_local(
        60,
        clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                let now = Local::now();
                let since = last_alert_check.replace(now);
                let Ok(events) = ui
                    .store
                    .events_between(since, now + ChronoDuration::days(2))
                else {
                    return glib::ControlFlow::Continue;
                };
                for event in crate::notify::due_alerts(&events, since, now) {
                    let notification = gio::Notification::new(&event.title);
                    notification.set_body(Some(&crate::notify::notification_body(&event, now)));
                    // Id'd by occurrence so a repeated send replaces instead
                    // of stacking, and each occurrence of a series alerts
                    // separately.
                    notify_app.send_notification(
                        Some(&format!("event-{}-{}", event.id, event.start.timestamp())),
                        &notification,
                    );
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    // Tooltips carry the accelerator: with no menu bar and no shortcuts
    // window yet, the control itself is the only place a binding can be
    // discovered.
    let today_button = gtk::Button::builder().label("Today").build();
    today_button.add_css_class("header-small");
    today_button.set_tooltip_text(Some("Jump to today (Ctrl+T)"));
    // Header-bar children default to valign fill, which stretches buttons to
    // the bar's full content height — natural (small) height needs center.
    today_button.set_valign(gtk::Align::Center);
    let prev_button = gtk::Button::from_icon_name("go-previous-symbolic");
    prev_button.set_tooltip_text(Some("Previous (Ctrl+Left)"));
    let next_button = gtk::Button::from_icon_name("go-next-symbolic");
    next_button.set_tooltip_text(Some("Next (Ctrl+Right)"));
    let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_box.add_css_class("linked");
    nav_box.append(&prev_button);
    nav_box.append(&next_button);

    let year_toggle = gtk::ToggleButton::builder()
        .label("Year")
        .active(initial_view_mode == ViewMode::Year)
        .build();
    let month_toggle = gtk::ToggleButton::builder()
        .label("Month")
        .group(&year_toggle)
        .active(initial_view_mode == ViewMode::Month)
        .build();
    let week_toggle = gtk::ToggleButton::builder()
        .label("Week")
        .group(&year_toggle)
        .active(initial_view_mode == ViewMode::Week)
        .build();
    let day_toggle = gtk::ToggleButton::builder()
        .label("Day")
        .group(&year_toggle)
        .active(initial_view_mode == ViewMode::Day)
        .build();
    year_toggle.set_tooltip_text(Some("Year view (Ctrl+1)"));
    month_toggle.set_tooltip_text(Some("Month view (Ctrl+2)"));
    week_toggle.set_tooltip_text(Some("Week view (Ctrl+3)"));
    day_toggle.set_tooltip_text(Some("Day view (Ctrl+4)"));

    let view_toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    view_toggle_box.add_css_class("linked");
    view_toggle_box.append(&year_toggle);
    view_toggle_box.append(&month_toggle);
    view_toggle_box.append(&week_toggle);
    view_toggle_box.append(&day_toggle);
    view_toggle_box.set_valign(gtk::Align::Center);
    for toggle in [&year_toggle, &month_toggle, &week_toggle, &day_toggle] {
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

    let search_button = gtk::Button::from_icon_name("system-search-symbolic");
    search_button.set_tooltip_text(Some("Search events (Ctrl+F)"));
    search_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |button| {
            let ui_for_pick = ui.clone();
            search::open(
                button,
                ui.store.clone(),
                Rc::new(move |event: Event| {
                    // Land on the event's day and let the normal render show
                    // it, rather than inventing a selection state the grid
                    // would then have to maintain.
                    ui_for_pick.state.borrow_mut().current_date = event.start.date_naive();
                    ui_for_pick.reset();
                }),
            );
        }
    ));

    let new_event_button = gtk::Button::from_icon_name("list-add-symbolic");
    new_event_button.set_tooltip_text(Some("New Event (Ctrl+N)"));
    new_event_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let start = next_half_hour();
            let ui2 = ui.clone();
            let ui_for_change = ui.clone();
            event_dialog::open(
                &ui.carousel,
                ui.store.clone(),
                create_targets(&ui),
                None,
                start,
                None,
                move |saved| ui2.apply_saved(saved),
                move |change| ui_for_change.record(change),
                None,
            );
        }
    ));

    // One primary action, one refresh, one way into the account list. Which
    // providers are connected is a property of the store, not of a control per
    // protocol, so nothing here is per-provider any more.
    let connect_account_button = gtk::Button::with_label("Connect an account");
    connect_account_button.add_css_class("suggested-action");
    connect_account_button.set_hexpand(true);
    connect_account_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| open_account_chooser(&ui, false)
    ));

    update_refresh_affordance(&ui);
    refresh_accounts_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| sync_connected_accounts_with_reporting(&ui)
    ));

    let manage_accounts_button = gtk::Button::builder()
        .label("Manage")
        .css_classes(["header-small", "flat"])
        .valign(gtk::Align::Center)
        .build();
    manage_accounts_button.set_tooltip_text(Some("View and remove connected accounts"));
    manage_accounts_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| open_manage_accounts_dialog(&ui)
    ));

    calendar_sidebar.append(&sidebar_actions(
        &connect_account_button,
        &refresh_accounts_button,
        &manage_accounts_button,
    ));
    calendar_sidebar.append(&mini_month);
    calendar_sidebar.append(&calendar_list);
    ui.reset_calendar_sidebar();
    ui.reset_mini_month();

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
            move || sync_connected_accounts(&ui)
        ),
    );
    glib::timeout_add_seconds_local(
        15 * 60,
        clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                sync_connected_accounts(&ui);
                glib::ControlFlow::Continue
            }
        ),
    );

    // The periodic timers above run on GLib's monotonic clock, which is frozen
    // while the machine is suspended — so after an overnight sleep they don't
    // fire at wake, they only resume counting down the remainder of their
    // interval. That leaves the grid parked on yesterday and unsynced until up
    // to a full interval of use later. Recover deterministically instead:
    // logind broadcasts `PrepareForSleep(b)` — true just before sleep, false
    // right after resume — so on the resume edge re-anchor "today"/now and kick
    // a background sync. gio speaks D-Bus, so this needs no new dependency; the
    // shared system-bus connection is kept alive by gio for the process's life,
    // which keeps the subscription live.
    if let Ok(system_bus) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE) {
        let subscription = system_bus.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            clone!(
                #[weak]
                ui,
                move |signal| {
                    // PrepareForSleep(b): `true` just before sleep, `false`
                    // right after resume. Only the resume edge matters; treat an
                    // unreadable payload as "going to sleep" so a malformed
                    // signal can't trigger a spurious sync.
                    if signal
                        .parameters
                        .child_value(0)
                        .get::<bool>()
                        .unwrap_or(true)
                    {
                        return;
                    }
                    ui.tick_clock();
                    sync_connected_accounts(&ui);
                }
            ),
        );
        // The subscription unsubscribes when dropped; there is exactly one, for
        // the whole process, so leak it to keep the resume listener alive.
        std::mem::forget(subscription);
    }

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
    header.pack_end(&search_button);
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

    // Each command re-triggers the control that already performs it rather
    // than repeating its body, so a shortcut can't drift from the button it
    // shadows. View switching activates the toggle instead of calling
    // `set_view_mode`, because the toggle is what the header draws — bypassing
    // it would change the grid while the header still claimed the old mode.
    //
    // The controller stays on the default (bubble) phase, so a focused entry
    // sees the key first and only unclaimed presses reach here.
    let key_controller = gtk::EventControllerKey::new();
    let ui_for_keys = ui.clone();
    key_controller.connect_key_pressed(clone!(
        #[strong]
        today_button,
        #[strong]
        prev_button,
        #[strong]
        next_button,
        #[strong]
        new_event_button,
        #[strong]
        search_button,
        #[strong]
        year_toggle,
        #[strong]
        month_toggle,
        #[strong]
        week_toggle,
        #[strong]
        day_toggle,
        move |_, key, _, state| {
            let Some(command) = key_command(key, state) else {
                return glib::Propagation::Proceed;
            };
            match command {
                KeyCommand::Undo => ui_for_keys.step_history(HistoryStep::Undo),
                KeyCommand::Redo => ui_for_keys.step_history(HistoryStep::Redo),
                KeyCommand::Today => today_button.emit_clicked(),
                KeyCommand::Previous => prev_button.emit_clicked(),
                KeyCommand::Next => next_button.emit_clicked(),
                KeyCommand::NewEvent => new_event_button.emit_clicked(),
                KeyCommand::Search => search_button.emit_clicked(),
                // Setting an already-active toggle emits nothing, which is
                // exactly the wanted no-op.
                KeyCommand::ViewYear => year_toggle.set_active(true),
                KeyCommand::ViewMonth => month_toggle.set_active(true),
                KeyCommand::ViewWeek => week_toggle.set_active(true),
                KeyCommand::ViewDay => day_toggle.set_active(true),
            }
            glib::Propagation::Stop
        }
    ));
    window.add_controller(key_controller);

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

    if show_window {
        window.present();
    }

    // With no remote accounts, make the next step visible without requiring
    // the user to discover the calendar sidebar first. Dismissing this leaves
    // local calendars fully usable; it returns on a later launch while there
    // are still no online accounts, matching the actual account state without
    // adding a speculative onboarding flag to storage.
    if show_window
        && ui
            .store
            .all_accounts()
            .is_ok_and(|accounts| accounts.is_empty())
    {
        glib::idle_add_local_once(clone!(
            #[strong]
            ui,
            move || open_account_chooser(&ui, true)
        ));
    }

    // Defer everything interactive until the carousel has actually been
    // allocated real geometry. Two problems otherwise: (1) scroll_to()
    // computes its jump as a pixel offset (position * width); called while
    // width is still 0 it silently resolves to an offset of 0 for any
    // target and never leaves the first page. (2) GTK's own startup
    // machinery (toggle-group resolution, the carousel's initial position
    // notify) fires a flurry of signals while the window first realizes;
    // connecting our handlers only after that settles keeps them from
    // being mistaken for real user input.
    //
    // The handlers are connected from the first rebuild's settle callback,
    // not from a timer. Geometry and handler activation then share one clock:
    // previously the rebuild centered itself on the frame clock while the
    // handlers went live on a fixed 125ms wall-clock delay, and on a slow or
    // frame-starved first paint that ordering inverted — `page-changed` was
    // live over a carousel still sitting on page 0, so startup read its own
    // uncentered position as a backward swipe and opened a week early.
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

            // Re-read the clock rather than trusting the date this closure
            // was built with: a window realizing slowly (or a machine waking
            // into a new day) should still open on today. A date asked for on
            // the command line is left alone — it was asked for.
            if date.is_none() {
                ui.state.borrow_mut().current_date = Local::now().date_naive();
            }
            // Installed before the rebuild so the settle loop can find it.
            // If something supersedes this rebuild before it lands, the
            // replacement inherits the pending action and connects instead —
            // it lives on `Ui`, not on one loop.
            ui.on_settled.replace(Some(Box::new(clone!(
                #[strong]
                ui,
                #[strong]
                today_button,
                #[strong]
                prev_button,
                #[strong]
                next_button,
                #[strong]
                year_toggle,
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
                        &year_toggle,
                        &month_toggle,
                        &week_toggle,
                        &day_toggle,
                        &zoom_box,
                        &zoom_out_button,
                        &zoom_in_button,
                    );
                }
            ))));
            ui.reset();
            glib::ControlFlow::Break
        }
    ));
}

fn sidebar_actions(
    connect_button: &gtk::Button,
    refresh_button: &gtk::Button,
    manage_button: &gtk::Button,
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
    title.set_hexpand(true);
    // Keep maintenance compact; connecting is the one primary task here.
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    title_row.append(&title);
    title_row.append(refresh_button);
    title_row.append(manage_button);
    section.append(&title_row);
    section.append(connect_button);
    connect_button.add_css_class("sidebar-action-button");

    section.upcast()
}

#[allow(clippy::too_many_arguments)]
fn connect_handlers(
    ui: &Rc<Ui>,
    today_button: &gtk::Button,
    prev_button: &gtk::Button,
    next_button: &gtk::Button,
    year_toggle: &gtk::ToggleButton,
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
            // Only a settled carousel can report a swipe. Unsettled, the
            // signal is fallout from our own centering — or from a rebuild
            // that hasn't landed — and acting on it moves the period twice.
            if !ui.sync.get().is_settled() {
                return;
            }
            let Some(delta) = swipe_delta(index) else {
                return;
            };
            // Claim the carousel before anything else, so the rest of this
            // swipe's signals land unsettled and are ignored — including
            // across the deferral below, which `advance` would otherwise
            // spend with the guard still down.
            ui.begin_rebuild();
            if delta > 0 {
                ui.advance(delta);
            } else {
                // `page-changed` is emitted while the swipe animation is still
                // running. Inserting a page ahead of the current one races
                // that animation, so let it finish first.
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
        move |_| ui.navigate(-1)
    ));

    next_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| ui.navigate(1)
    ));

    for (toggle, mode) in [
        (year_toggle, ViewMode::Year),
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
    // Only the timed views have a day to stretch; month and year are grids of
    // dates with no hour axis to zoom.
    zoom_box.set_visible(!matches!(state.view_mode, ViewMode::Month | ViewMode::Year));
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
            let Some(config) = ui.config.borrow().google.clone() else {
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
                    ui.config.borrow().google.clone(),
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
        let original = event.draft();
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
                            Ok(Ok(())) => {
                                ui.record(undo::Change::edited(
                                    event.calendar_id,
                                    event.id,
                                    original.clone(),
                                    draft.clone(),
                                ));
                                glib::ControlFlow::Break
                            }
                            Ok(Err(error)) => {
                                undo_failed_drag(&ui, event.id, &draft, &original);
                                ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                                    "Couldn't move event: {error}"
                                )));
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                undo_failed_drag(&ui, event.id, &draft, &original);
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
                    ui.record(undo::Change::edited(
                        event.calendar_id,
                        event.id,
                        original,
                        draft,
                    ));
                    ui.reset();
                }
            }
        }
    })
}

/// Puts back the local write a failed drag made, unless the event has changed
/// since — see [`undo_drag`].
fn undo_failed_drag(ui: &Rc<Ui>, event_id: i64, optimistic: &EventDraft, original: &EventDraft) {
    let Ok(Some(current)) = ui.store.event_by_id(event_id) else {
        return;
    };
    let Some(undo) = undo_drag(&current, optimistic, original) else {
        return;
    };
    if ui.store.update_event(event_id, &undo).is_ok() {
        ui.reset();
    }
}

/// The write that undoes a failed drag, or `None` when the event has moved on.
///
/// A drag writes its new time to SQLite immediately and only learns the remote
/// move failed later, by which point the user may have dragged the same event
/// again. Undoing then would restore the state from before *both* moves and
/// throw away the newer one — which, being remote, has already succeeded. So
/// the undo only applies while the row still holds what this drag wrote, and
/// it puts back just the times: anything else edited since belongs to whoever
/// edited it.
fn undo_drag(
    current: &Event,
    optimistic: &EventDraft,
    original: &EventDraft,
) -> Option<EventDraft> {
    let unchanged = current.start == optimistic.start
        && current.end == optimistic.end
        && current.all_day == optimistic.all_day;
    if !unchanged {
        return None;
    }
    Some(EventDraft {
        start: original.start,
        end: original.end,
        all_day: original.all_day,
        ..current.draft()
    })
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
        recurrence: event.recurrence,
        reminder_minutes: event.reminder_minutes,
        attendees: event.attendees.clone(),
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
        recurrence: event.recurrence,
        reminder_minutes: event.reminder_minutes,
        attendees: event.attendees.clone(),
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
        recurrence: event.recurrence,
        reminder_minutes: event.reminder_minutes,
        attendees: event.attendees.clone(),
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

/// Toast text for a completed Add. The account is stored before its first sync
/// runs, so a sync failure reports as an added account whose calendars are
/// still empty — not as a failed connection.
fn add_summary(outcome: &Result<SyncOutcome, String>, display_name: &str, noun: &str) -> String {
    match outcome {
        Ok(outcome) => outcome.added_summary(display_name, noun),
        Err(error) => sync::added_but_not_synced(display_name, first_line(error)),
    }
}

/// Points the one Refresh control at what is actually happening. Called
/// whenever a sync starts or ends, whichever way it ended.
fn update_refresh_affordance(ui: &Rc<Ui>) {
    let busy = ui.activity.any_sync_in_flight();
    ui.refresh_accounts_button.set_sensitive(!busy);
    ui.refresh_accounts_button.set_tooltip_text(Some(if busy {
        "Refreshing connected accounts…"
    } else {
        "Refresh all connected accounts"
    }));
}

fn has_accounts(ui: &Rc<Ui>, provider: Provider) -> bool {
    ui.store
        .accounts_for_provider(provider.key)
        .map(|accounts| !accounts.is_empty())
        .unwrap_or(false)
}

/// A completed Add. The account row and its secret are written before the
/// initial sync runs, so `outcome` carries that sync's failure separately: the
/// account exists either way, and reporting the whole Add as failed would hide
/// it.
struct GoogleAddResult {
    display_name: String,
    outcome: Result<SyncOutcome, String>,
}

/// Runs the interactive OAuth flow for a Google account, identifies the
/// signed-in account from its primary calendar, saves that account-specific
/// refresh token, and immediately performs an initial sync.
///
/// `expected_account` is set when this is the account center's "Update
/// sign-in": whoever signs in must be the account whose row was clicked, or
/// nothing is written. Signing in as somebody else there used to quietly
/// create a second account instead of updating the one the user picked.
fn add_google_account(ui: &Rc<Ui>, expected_account: Option<String>) {
    let Some(google_config) = ui.config.borrow().google.clone() else {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Add a Google OAuth client to ~/.config/calix/config.toml first — see the README",
        ));
        return;
    };

    // The browser takes over from here and this window keeps no dialog of its
    // own, so the guard and the toast are the only things telling the user a
    // sign-in is already under way.
    if !ui.activity.start_sign_in() {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "A Google sign-in is already open in your browser",
        ));
        return;
    }
    ui.toast_overlay.add_toast(adw::Toast::new(
        "Opening your browser to sign in to Google…",
    ));

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<GoogleAddResult, String> {
            let tokens = google::oauth::sign_in(&google_config).map_err(|e| e.to_string())?;
            let (provider_account_id, display_name) =
                google::sync::account_identity(&tokens.access_token)?;
            // Checked before anything is written, so a mismatch costs nothing
            // but the message.
            if let Some(expected) = &expected_account
                && expected != &provider_account_id
            {
                return Err(format!(
                    "Signed in as {provider_account_id}, but this was an update for {expected}. \
                     Nothing was changed — use Connect an account to add {provider_account_id}."
                ));
            }
            let token_key = google::oauth::token_key(&provider_account_id);
            let store = Store::open().map_err(|e| e.to_string())?;
            // Secret first: a row whose credential failed to save is an account
            // that can never sync, while an unreferenced secret is inert and is
            // overwritten by the next attempt.
            google::oauth::save_refresh_token(&token_key, &tokens.refresh_token)
                .map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_google_account(&provider_account_id, &display_name, &token_key)
                .map_err(|e| e.to_string())?;
            let outcome = google::sync::sync_account(&tokens.access_token, &store, account_id);
            record_sync_result(&store, account_id, &display_name, &outcome);
            Ok(GoogleAddResult {
                display_name,
                outcome,
            })
        })();
        let _ = tx.send(result);
    });

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(
            #[strong]
            ui,
            move || {
                let message = match rx.try_recv() {
                    Ok(Ok(result)) => {
                        ui.reset_calendar_sidebar();
                        ui.reset();
                        add_summary(&result.outcome, &result.display_name, "calendar")
                    }
                    Ok(Err(error)) => format!("Google connect failed: {}", first_line(&error)),
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    // The worker can only have panicked; say so rather than
                    // leaving the sign-in looking like it is still going.
                    Err(mpsc::TryRecvError::Disconnected) => {
                        "The Google sign-in stopped unexpectedly. Try again.".to_string()
                    }
                };
                ui.activity.finish_sign_in();
                ui.toast_overlay
                    .add_toast(adw::Toast::new(&glib::markup_escape_text(&message)));
                glib::ControlFlow::Break
            }
        ),
    );
}

/// A completed Add — see [`GoogleAddResult`] for why the initial sync's/// A completed Add — see [`GoogleAddResult`] for why the initial sync's
/// failure travels separately from the Add's.
struct CaldavAddResult {
    display_name: String,
    outcome: Result<SyncOutcome, String>,
}

fn chooser_row(title: &str, subtitle: &str, action: impl Fn() + 'static) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    let button = gtk::Button::builder()
        .label("Connect")
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    button.connect_clicked(move |_| action());
    row
}

fn open_account_chooser(ui: &Rc<Ui>, welcome: bool) {
    let dialog = adw::Dialog::builder()
        .title(if welcome {
            "Welcome to Calix"
        } else {
            "Connect an account"
        })
        .content_width(500)
        .build();
    let close_button = gtk::Button::with_label("Close");
    let header = adw::HeaderBar::new();
    if !welcome {
        header.pack_start(&close_button);
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    if welcome {
        let intro = gtk::Label::new(Some(
            "Connect a calendar account to see and edit its events here. You can also keep using Calix with local calendars only.",
        ));
        intro.set_wrap(true);
        intro.set_xalign(0.0);
        content.append(&intro);
    }

    let group = adw::PreferencesGroup::builder()
        .title("Choose a service")
        .build();
    // `dialog` is captured weakly throughout: each row's button lives inside
    // the dialog, so holding it strongly here made a cycle the dialog could
    // never be freed from. It is alive for as long as it is presented, which
    // is the only time a row can be activated.
    group.add(&chooser_row(
        "Google Calendar",
        if ui.config.borrow().google.is_some() {
            "Sign in securely in your web browser"
        } else {
            "Advanced setup required before browser sign-in"
        },
        clone!(
            #[strong]
            ui,
            #[weak]
            dialog,
            move || {
                dialog.close();
                if ui.config.borrow().google.is_none() {
                    open_google_setup_help(&ui);
                    return;
                }
                add_google_account(&ui, None);
            }
        ),
    ));
    group.add(&chooser_row(
        "Apple iCloud",
        "Use an app-specific password from your Apple Account",
        clone!(
            #[strong]
            ui,
            #[weak]
            dialog,
            move || {
                dialog.close();
                open_icloud_account_dialog(&ui, None);
            }
        ),
    ));
    group.add(&chooser_row(
        "Fastmail",
        "Connect calendars using your Fastmail app password",
        clone!(
            #[strong]
            ui,
            #[weak]
            dialog,
            move || {
                dialog.close();
                open_caldav_account_dialog(
                    &ui,
                    "Fastmail",
                    Some("https://caldav.fastmail.com"),
                    None,
                );
            }
        ),
    ));
    group.add(&chooser_row(
        "Nextcloud",
        "Enter the address of your Nextcloud server",
        clone!(
            #[strong]
            ui,
            #[weak]
            dialog,
            move || {
                dialog.close();
                open_caldav_account_dialog(&ui, "Nextcloud", None, None);
            }
        ),
    ));
    group.add(&chooser_row(
        "Other calendar server",
        "Advanced: connect a CalDAV-compatible service",
        clone!(
            #[strong]
            ui,
            #[weak]
            dialog,
            move || {
                dialog.close();
                open_caldav_account_dialog(&ui, "Other calendar server", None, None);
            }
        ),
    ));
    content.append(&group);
    let privacy = gtk::Label::new(Some(
        "Passwords and sign-in tokens are stored in your system keyring. Calendar events are cached on this computer.",
    ));
    privacy.set_wrap(true);
    privacy.set_xalign(0.0);
    privacy.add_css_class("dim-label");
    content.append(&privacy);
    if welcome {
        let local_only = gtk::Button::with_label("Continue with local calendar");
        local_only.set_halign(gtk::Align::Center);
        local_only.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        content.append(&local_only);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    close_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.present(Some(&ui.carousel));
}

fn open_google_setup_help(ui: &Rc<Ui>) {
    let dialog = adw::Dialog::builder()
        .title("Set up Google Calendar")
        .content_width(480)
        .build();
    let close = gtk::Button::with_label("Close");
    let header = adw::HeaderBar::new();
    header.pack_start(&close);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let explanation = gtk::Label::new(Some(
        "Google requires this development build to use your own OAuth app credentials. This is an advanced, one-time setup in Google Cloud.",
    ));
    explanation.set_wrap(true);
    explanation.set_xalign(0.0);
    content.append(&explanation);
    if let Some(error) = ui.config.borrow().load_error.as_deref() {
        let error_group = adw::PreferencesGroup::builder()
            .title("Configuration needs attention")
            .description(error)
            .build();
        content.append(&error_group);
    }
    let steps = gtk::Label::new(Some(
        "1. Create a Desktop app OAuth client and enable Google Calendar API.\n\
         2. Paste the client ID and client secret below.\n\
         3. Save and continue to sign in with Google.",
    ));
    steps.set_wrap(true);
    steps.set_xalign(0.0);
    content.append(&steps);
    let credentials = adw::PreferencesGroup::builder()
        .title("Google OAuth client")
        .description("Saved only in ~/.config/calix/config.toml with owner-only permissions")
        .build();
    let client_id = adw::EntryRow::builder().title("Client ID").build();
    let client_secret = adw::PasswordEntryRow::builder()
        .title("Client secret")
        .build();
    if let Some(existing) = ui.config.borrow().google.as_ref() {
        client_id.set_text(&existing.client_id);
        client_secret.set_text(&existing.client_secret);
    }
    credentials.add(&client_id);
    credentials.add(&client_secret);
    content.append(&credentials);
    let save_error = gtk::Label::new(None);
    save_error.add_css_class("error");
    save_error.set_wrap(true);
    save_error.set_xalign(0.0);
    save_error.set_visible(false);
    content.append(&save_error);
    let save = gtk::Button::with_label("Save and sign in");
    save.add_css_class("suggested-action");
    save.set_halign(gtk::Align::Start);
    content.append(&save);
    let warning = gtk::Label::new(Some(
        "If the OAuth app remains in Google's Testing mode, Google may require you to sign in again periodically.",
    ));
    warning.set_wrap(true);
    warning.set_xalign(0.0);
    warning.add_css_class("dim-label");
    content.append(&warning);
    content.append(
        &gtk::LinkButton::builder()
            .label("Open the complete Google setup guide")
            .uri("https://github.com/ianswope/calix#connecting-google-calendar")
            .halign(gtk::Align::Start)
            .build(),
    );
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    close.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    save.connect_clicked(clone!(
        #[strong]
        ui,
        #[weak]
        dialog,
        #[weak]
        client_id,
        #[weak]
        client_secret,
        #[weak]
        save_error,
        move |_| match Config::save_google(&client_id.text(), &client_secret.text()) {
            Ok(config) => {
                *ui.config.borrow_mut() = config;
                dialog.close();
                add_google_account(&ui, None);
            }
            Err(error) => {
                save_error.set_label(&error);
                save_error.set_visible(true);
            }
        }
    ));
    dialog.present(Some(&ui.carousel));
}

fn show_startup_error(app: &adw::Application, error: &str, date: Option<NaiveDate>) {
    let data_directory = crate::xdg::data_home().join("calix");
    let open_directory = data_directory
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or_else(|| std::path::Path::new("/"))
        .to_path_buf();
    let diagnostic = format!(
        "Calix could not open its local calendar database.\n\nDatabase folder: {}\nError: {error}",
        data_directory.display()
    );
    let status = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Calix couldn’t open your calendars")
        .description("Your data has not been deleted. Check the database folder’s permissions, then retry. You can copy the diagnostic below when asking for help.")
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::Center);

    let retry = gtk::Button::with_label("Retry");
    retry.add_css_class("suggested-action");
    let open_folder = gtk::Button::with_label("Open data location");
    let copy = gtk::Button::with_label("Copy diagnostic");
    actions.append(&retry);
    actions.append(&open_folder);
    actions.append(&copy);
    status.set_child(Some(&actions));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Calix")
        .default_width(560)
        .default_height(360)
        .content(&status)
        .build();
    retry.connect_clicked(clone!(
        #[weak]
        window,
        #[weak]
        app,
        move |_| {
            window.close();
            build(&app, date, true);
        }
    ));
    open_folder.connect_clicked(move |_| {
        let Ok(uri) = url::Url::from_directory_path(&open_directory) else {
            eprintln!(
                "calix: could not make a file URL for {}",
                open_directory.display()
            );
            return;
        };
        if let Err(error) = gtk::gio::AppInfo::launch_default_for_uri(
            uri.as_str(),
            None::<&gtk::gio::AppLaunchContext>,
        ) {
            eprintln!("calix: could not open database folder: {error}");
        }
    });
    copy.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&diagnostic);
        }
    });
    window.present();
}

/// Forgets `account`: its keyring credential, then its events, calendars, and
/// row. Local only — nothing is deleted or revoked with the provider, so
/// re-adding the same account later re-syncs it.
fn disconnect_account(store: &Store, account: &store::Account) -> Result<(), String> {
    // Clear the credential first. If this fails the account stays listed, which
    // is the recoverable order: a leftover row can be removed again, whereas a
    // secret orphaned by a deleted row has no UI left to reach it.
    match account.provider.as_str() {
        "google" => google::oauth::delete_refresh_token(&account.token_key)
            .map_err(|e| format!("couldn't remove the saved sign-in: {e}"))?,
        _ => icloud::credentials::delete_password(&account.token_key)
            .map_err(|e| format!("couldn't remove the saved password: {e}"))?,
    }
    store
        .delete_account(account.id)
        .map_err(|e| format!("couldn't remove the account: {e}"))
}

/// Lists every connected account with a Remove button. Removal asks for
/// confirmation first, since it drops the account's cached events.
fn open_manage_accounts_dialog(ui: &Rc<Ui>) {
    let dialog = adw::Dialog::builder()
        .title("Accounts")
        .content_width(460)
        .build();

    let close_button = gtk::Button::with_label("Close");
    let header = adw::HeaderBar::new();
    header.pack_start(&close_button);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let behavior = adw::PreferencesGroup::builder()
        .title("Background behavior")
        .build();
    let background_alerts = adw::SwitchRow::builder()
        .title("Start Calix when you sign in")
        .subtitle("Keep calendar sync and event alerts working after the window is closed")
        .active(crate::autostart::enabled())
        .build();
    let reverting_background_alerts = Rc::new(Cell::new(false));
    background_alerts.connect_active_notify(clone!(
        #[strong]
        ui,
        #[strong]
        reverting_background_alerts,
        move |row| {
            if reverting_background_alerts.replace(false) {
                return;
            }
            if let Err(error) = crate::autostart::set_enabled(row.is_active()) {
                reverting_background_alerts.set(true);
                row.set_active(!row.is_active());
                ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                    "Could not update background startup: {error}"
                )));
            }
        }
    ));
    behavior.add(&background_alerts);
    content.append(&behavior);

    let accounts = ui.store.all_accounts().unwrap_or_default();
    if accounts.is_empty() {
        let empty = gtk::Label::new(Some(
            "No online accounts are connected. Choose Connect an account to get started.",
        ));
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        empty.add_css_class("dim-label");
        content.append(&empty);
    } else {
        let group = adw::PreferencesGroup::new();
        for account in accounts {
            let calendar_count = ui
                .store
                .calendars_for_account(account.id)
                .map(|c| c.len())
                .unwrap_or(0);
            let provider_label = friendly_account_provider(&account);
            let sync_status = account_sync_status(&account);
            let subtitle = format!(
                "{} · {} calendar{} · {}",
                provider_label,
                calendar_count,
                if calendar_count == 1 { "" } else { "s" },
                sync_status,
            );
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&account.display_name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();
            row.set_title_lines(1);
            row.set_subtitle_lines(1);

            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let retry_button = gtk::Button::builder()
                .icon_name("view-refresh-symbolic")
                .tooltip_text("Refresh this provider's connected accounts")
                .valign(gtk::Align::Center)
                .build();
            retry_button.connect_clicked(clone!(
                #[strong]
                ui,
                #[strong]
                account,
                move |_| retry_account_provider(&ui, &account)
            ));
            let update_button = gtk::Button::builder()
                .label(if account.provider == "google" {
                    "Update sign-in"
                } else {
                    "Update password"
                })
                .valign(gtk::Align::Center)
                .build();
            update_button.connect_clicked(clone!(
                #[strong]
                ui,
                #[strong]
                account,
                #[weak]
                dialog,
                move |_| {
                    dialog.close();
                    update_account_credentials(&ui, &account);
                }
            ));
            let remove_button = gtk::Button::builder()
                .icon_name("edit-delete-symbolic")
                .tooltip_text("Disconnect account")
                .valign(gtk::Align::Center)
                .css_classes(["destructive-action"])
                .build();
            remove_button.connect_clicked(clone!(
                #[strong]
                ui,
                #[weak]
                dialog,
                move |_| {
                    confirm_disconnect_account(&ui, &dialog, &account);
                }
            ));
            actions.append(&retry_button);
            actions.append(&update_button);
            actions.append(&remove_button);
            row.add_suffix(&actions);
            group.add(&row);
        }
        content.append(&group);
    }

    let note = gtk::Label::new(Some(
        "Removing an account deletes its saved credential and cached events from \
         this computer only. Nothing is changed or revoked on the provider, so you \
         can add it back at any time.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    content.append(&note);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    close_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    dialog.present(Some(&ui.carousel));
}

fn friendly_account_provider(account: &store::Account) -> String {
    match account.provider.as_str() {
        "google" => "Google Calendar".to_string(),
        "icloud" => "Apple iCloud".to_string(),
        "caldav" => {
            let host = account
                .server_url
                .as_deref()
                .map(host_label)
                .unwrap_or_default();
            if host.contains("fastmail") {
                "Fastmail".to_string()
            } else if host.contains("nextcloud") {
                "Nextcloud".to_string()
            } else if host.is_empty() {
                "Calendar server".to_string()
            } else {
                format!("Calendar server · {host}")
            }
        }
        other => Provider::label_for_key(other).to_string(),
    }
}

fn account_sync_status(account: &store::Account) -> String {
    if let Some(error) = account.last_sync_error.as_deref() {
        return format!("Needs attention: {}", first_line(error));
    }
    let Some(timestamp) = account.last_sync_at.as_deref() else {
        return "Waiting for first sync".to_string();
    };
    let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp) else {
        // A row we can't read the time out of is a row with something wrong in
        // it; "Synced automatically" read as a clean bill of health.
        return "Last sync time unreadable".to_string();
    };
    let local = timestamp.with_timezone(&Local);
    format!("Last updated {}", local.format("%b %-d, %-I:%M %p"))
}

fn sync_account_with_health(
    store: &Store,
    account: &store::Account,
    sync: impl FnOnce() -> Result<SyncOutcome, String>,
) -> Result<SyncOutcome, String> {
    let result = sync();
    record_sync_result(store, account.id, &account.label(), &result);
    result
}

fn record_sync_result(
    store: &Store,
    account_id: i64,
    account_label: &str,
    result: &Result<SyncOutcome, String>,
) {
    let error = match &result {
        Ok(outcome) => outcome.failure_note(),
        Err(error) => Some(error.clone()),
    };
    if let Err(record_error) = store.record_account_sync(account_id, error.as_deref()) {
        eprintln!(
            "calix: could not record sync status for {}: {record_error}",
            account_label
        );
    }
}

fn retry_account_provider(ui: &Rc<Ui>, account: &store::Account) {
    // Sync is provider-wide in the current backend. Say so in the tooltip, but
    // route the action from the affected account so recovery is discoverable.
    match account.provider.as_str() {
        "google" => sync_google_accounts(ui, false),
        "icloud" => sync_icloud_accounts(ui, false),
        "caldav" => sync_caldav_accounts(ui, false),
        _ => ui.toast_overlay.add_toast(adw::Toast::new(
            "This account type cannot be refreshed by this version of Calix",
        )),
    }
}

fn update_account_credentials(ui: &Rc<Ui>, account: &store::Account) {
    match account.provider.as_str() {
        "google" => add_google_account(ui, Some(account.provider_account_id.clone())),
        "icloud" => open_icloud_account_dialog(ui, Some(&account.provider_account_id)),
        "caldav" => {
            open_caldav_account_dialog(
                ui,
                &friendly_account_provider(account),
                account.server_url.as_deref(),
                Some(&account.provider_account_id),
            );
        }
        _ => ui.toast_overlay.add_toast(adw::Toast::new(
            "This account type cannot be updated by this version of Calix",
        )),
    }
}

/// Confirms before disconnecting, naming what is and isn't affected.
fn confirm_disconnect_account(ui: &Rc<Ui>, parent: &adw::Dialog, account: &store::Account) {
    let alert = adw::AlertDialog::builder()
        .heading(format!("Remove {}?", account.display_name))
        .body(format!(
            "Calix will forget this {} account's saved credential and delete its \
             cached calendars and events from this computer.\n\nYour calendars and \
             events are not touched on the provider, and no password or token is \
             revoked there.",
            Provider::label_for_key(&account.provider)
        ))
        .build();
    alert.add_response("cancel", "Cancel");
    alert.add_response("remove", "Remove");
    alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");

    alert.connect_response(
        None,
        clone!(
            #[strong]
            ui,
            #[strong]
            account,
            #[weak]
            parent,
            move |_, response| {
                if response != "remove" {
                    return;
                }
                match disconnect_account(&ui.store, &account) {
                    Ok(()) => {
                        ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                            "Removed {}",
                            account.display_name
                        )));
                        ui.reset_calendar_sidebar();
                        ui.reset();
                        parent.close();
                        // Reopen so the list reflects the removal and more
                        // accounts can be removed without re-navigating.
                        // The account center can be reopened from the sidebar;
                        // avoid retaining stale sync controls in this callback.
                    }
                    Err(error) => {
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&glib::markup_escape_text(&error)));
                    }
                }
            }
        ),
    );

    alert.present(Some(parent));
}

fn open_icloud_account_dialog(ui: &Rc<Ui>, apple_id_hint: Option<&str>) {
    let dialog = adw::Dialog::builder()
        .title("Apple iCloud")
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
    if let Some(apple_id) = apple_id_hint {
        apple_id_row.set_text(apple_id);
    }
    let password_row = adw::PasswordEntryRow::builder()
        .title("App-Specific Password")
        .build();

    let group = adw::PreferencesGroup::new();
    group.add(&apple_id_row);
    group.add(&password_row);

    // Spelled out as steps rather than one line of prose: the password is
    // usually generated on a phone while this dialog sits on the desktop, so
    // the instructions have to survive being read from across the room.
    let note = gtk::Label::new(Some(
        "iCloud needs an app-specific password — not your Apple Account password.\n\
         1. Open Apple Account settings below and sign in.\n\
         2. Under Sign-In and Security, choose App-Specific Passwords.\n\
         3. Generate one for Calix and paste it here.",
    ));
    // `set_wrap` alone isn't enough: a wrapping GtkLabel still requests its
    // full natural width, so a long line silently widens the dialog past the
    // 420px it asked for. Capping the character width is what makes it
    // actually wrap instead.
    const PROSE_WIDTH_CHARS: i32 = 44;

    note.set_wrap(true);
    note.set_max_width_chars(PROSE_WIDTH_CHARS);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");

    let link_button = gtk::LinkButton::builder()
        .label("Open Apple Account settings")
        .uri(icloud::APP_PASSWORD_URL)
        .halign(gtk::Align::Start)
        .build();

    // Hint, not a gate: it appears while the field doesn't look like Apple's
    // format, but never blocks Connect. See `normalize_app_password`.
    let hint_label = gtk::Label::new(None);
    hint_label.set_wrap(true);
    hint_label.set_max_width_chars(PROSE_WIDTH_CHARS);
    hint_label.set_xalign(0.0);
    hint_label.add_css_class("dim-label");
    hint_label.set_visible(false);

    let error_label = gtk::Label::new(None);
    error_label.set_wrap(true);
    error_label.set_max_width_chars(PROSE_WIDTH_CHARS);
    error_label.set_xalign(0.0);
    error_label.add_css_class("error");
    error_label.set_visible(false);

    password_row.connect_changed(clone!(
        #[weak]
        hint_label,
        move |row| {
            let text = row.text();
            let unrecognized =
                !text.trim().is_empty() && icloud::normalize_app_password(&text).is_none();
            hint_label.set_label(
                "That doesn't look like an app-specific password — Apple's are \
                 sixteen letters, shown like abcd-efgh-ijkl-mnop. Connecting will \
                 still try it.",
            );
            hint_label.set_visible(unrecognized);
        }
    ));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&group);
    content.append(&hint_label);
    content.append(&error_label);
    content.append(&note);
    content.append(&link_button);

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
        #[weak]
        dialog,
        #[weak]
        error_label,
        #[strong]
        connect_button,
        move |_| {
            let apple_id = apple_id_row.text().trim().to_string();
            let typed = password_row.text().to_string();
            if apple_id.is_empty() || typed.trim().is_empty() {
                // Previously a silent `return`, which read as a dead button.
                error_label
                    .set_label("Enter your Apple Account email and an app-specific password.");
                error_label.set_visible(true);
                return;
            }
            error_label.set_visible(false);
            // Send the canonical form when we recognize one, and the raw input
            // when we don't — an unfamiliar format is still worth attempting.
            let app_password =
                icloud::normalize_app_password(&typed).unwrap_or_else(|| typed.trim().to_string());
            add_icloud_account(
                &ui,
                &dialog,
                &error_label,
                &connect_button,
                apple_id,
                app_password,
            );
        }
    ));

    dialog.present(Some(&ui.carousel));
}

/// Connects an iCloud account, keeping `dialog` open until it succeeds.
///
/// The dialog used to close on click, before the network attempt: a mistyped
/// password then threw away both fields and reported itself as a toast over an
/// empty screen, so correcting a single character meant retyping everything.
/// Holding it open until the credentials actually verify makes a failed attempt
/// cost one edit.
fn add_icloud_account(
    ui: &Rc<Ui>,
    dialog: &adw::Dialog,
    error_label: &gtk::Label,
    connect_button: &gtk::Button,
    apple_id: String,
    app_password: String,
) {
    connect_button.set_sensitive(false);
    connect_button.set_label("Connecting…");

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
            // Secret before row — see the Google flow.
            icloud::credentials::save_app_password(&token_key, &credentials.password)
                .map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_icloud_account(&apple_id, &apple_id, &token_key)
                .map_err(|e| e.to_string())?;
            let outcome = caldav::sync_account(&credentials, &store, account_id);
            record_sync_result(&store, account_id, &apple_id, &outcome);
            Ok(CaldavAddResult {
                display_name: apple_id,
                outcome,
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
            dialog,
            #[strong]
            error_label,
            #[strong]
            connect_button,
            move || {
                let restore = || {
                    connect_button.set_label("Connect");
                    connect_button.set_sensitive(true);
                };
                match rx.try_recv() {
                    Ok(Ok(result)) => {
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&glib::markup_escape_text(&add_summary(
                                &result.outcome,
                                &result.display_name,
                                "iCloud calendar",
                            ))));
                        restore();
                        dialog.close();
                        ui.reset_calendar_sidebar();
                        ui.reset();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(error)) => {
                        // Inline, with the dialog still up and both fields
                        // intact, rather than a toast over an empty screen.
                        error_label.set_label(first_line(&error));
                        error_label.set_visible(true);
                        restore();
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        error_label
                            .set_label("The connection attempt stopped unexpectedly. Try again.");
                        error_label.set_visible(true);
                        restore();
                        glib::ControlFlow::Break
                    }
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
fn sync_connected_accounts(ui: &Rc<Ui>) {
    sync_connected_accounts_mode(ui, true);
}

fn sync_connected_accounts_with_reporting(ui: &Rc<Ui>) {
    if ui
        .store
        .all_accounts()
        .is_ok_and(|accounts| accounts.is_empty())
    {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Connect an account before refreshing online calendars",
        ));
        return;
    }
    sync_connected_accounts_mode(ui, false);
}

fn sync_connected_accounts_mode(ui: &Rc<Ui>, quiet: bool) {
    // A provider already mid-sync is skipped rather than queued behind itself;
    // `run_account_sync` refuses a second one anyway, but skipping here keeps an
    // automatic pass from toasting about it.
    type SyncFn = fn(&Rc<Ui>, bool);
    let providers: [(Provider, SyncFn); 3] = [
        (provider::GOOGLE, sync_google_accounts),
        (provider::ICLOUD, sync_icloud_accounts),
        (provider::CALDAV, sync_caldav_accounts),
    ];
    for (provider, sync) in providers {
        if !ui.activity.is_syncing(provider) && has_accounts(ui, provider) {
            sync(ui, quiet);
        }
    }
}

/// The shared half of every sync: claim the provider, run `load_and_sync` on a
/// worker thread, poll for its result, then report and release the claim.
///
/// `quiet` suppresses the success toast (errors are always surfaced) so
/// automatic launch/periodic syncs don't nag; manual clicks pass `false`.
/// `load_and_sync` returns how many accounts it tried along with the outcome —
/// it runs off the main thread, so it opens its own `Store`.
fn run_account_sync<F>(ui: &Rc<Ui>, quiet: bool, provider: Provider, load_and_sync: F)
where
    F: FnOnce() -> Result<(usize, SyncOutcome), String> + Send + 'static,
{
    // One sync per provider at a time: two would race each other's writes and
    // double every toast.
    if !ui.activity.start_sync(provider) {
        return;
    }
    update_refresh_affordance(ui);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(load_and_sync());
    });

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(
            #[strong]
            ui,
            move || {
                let outcome = match rx.try_recv() {
                    Ok(result) => result,
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    // The worker died without sending — it can only have
                    // panicked. Say so rather than quietly going idle again:
                    // nothing synced, and the log line is the only trace the
                    // panic leaves.
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let message = provider.sync_failed("it stopped unexpectedly");
                        eprintln!("calix: {message}");
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&glib::markup_escape_text(&message)));
                        ui.activity.finish_sync(provider);
                        update_refresh_affordance(&ui);
                        return glib::ControlFlow::Break;
                    }
                };

                match outcome {
                    Ok((account_count, outcome)) => {
                        if outcome.needs_reporting(quiet) {
                            ui.toast_overlay.add_toast(adw::Toast::new(
                                &outcome.synced_summary(provider.calendar_noun, account_count),
                            ));
                        }
                        ui.reset_calendar_sidebar();
                        ui.reset();
                    }
                    Err(error) => {
                        eprintln!("calix: {}", provider.sync_failed(&error));
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&glib::markup_escape_text(
                                &provider.sync_failed(first_line(&error)),
                            )));
                    }
                }

                ui.activity.finish_sync(provider);
                update_refresh_affordance(&ui);
                glib::ControlFlow::Break
            }
        ),
    );
}

fn sync_google_accounts(ui: &Rc<Ui>, quiet: bool) {
    let Some(google_config) = ui.config.borrow().google.clone() else {
        ui.toast_overlay.add_toast(adw::Toast::new(
            "Add a Google OAuth client to ~/.config/calix/config.toml first — see the README",
        ));
        return;
    };

    run_account_sync(ui, quiet, provider::GOOGLE, move || {
        let store = Store::open().map_err(|e| e.to_string())?;
        let mut accounts = store.google_accounts().map_err(|e| e.to_string())?;
        // A token saved before accounts were per-account gets adopted here, so
        // an upgrade doesn't look like a disconnected account.
        if accounts.is_empty()
            && let Some(token) =
                google::oauth::get_access_token(&google_config, google::oauth::legacy_token_key())
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
            return Err(provider::GOOGLE.none_connected());
        }

        let account_count = accounts.len();
        let outcome = sync::sync_accounts(
            &accounts,
            |account| account.label(),
            |account| {
                sync_account_with_health(&store, account, || {
                    // `AuthError` already words this as "open Accounts and
                    // choose Update sign-in"; it used to be patched here too.
                    let token = google::oauth::get_access_token(&google_config, &account.token_key)
                        .map_err(|e| e.to_string())?
                        .ok_or("no saved sign-in — open Accounts and choose Update sign-in")?;
                    google::sync::sync_account(&token, &store, account.id)
                })
            },
        );
        Ok((account_count, outcome))
    });
}

fn sync_icloud_accounts(ui: &Rc<Ui>, quiet: bool) {
    run_account_sync(ui, quiet, provider::ICLOUD, || {
        let store = Store::open().map_err(|e| e.to_string())?;
        let accounts = store.icloud_accounts().map_err(|e| e.to_string())?;
        if accounts.is_empty() {
            return Err(provider::ICLOUD.none_connected());
        }

        let account_count = accounts.len();
        let outcome = sync::sync_accounts(
            &accounts,
            |account| account.label(),
            |account| {
                sync_account_with_health(&store, account, || {
                    let app_password = icloud::credentials::app_password(&account.token_key)
                        .map_err(|e| e.to_string())?
                        .ok_or(
                            "no saved app-specific password — open Accounts and choose Update password",
                        )?;
                    let credentials = caldav::Credentials {
                        base_url: icloud::ICLOUD_CALDAV_ROOT.to_string(),
                        username: account.provider_account_id.clone(),
                        password: app_password,
                    };
                    caldav::sync_account(&credentials, &store, account.id)
                })
            },
        );
        Ok((account_count, outcome))
    });
}

fn open_caldav_account_dialog(
    ui: &Rc<Ui>,
    service_name: &str,
    server_hint: Option<&str>,
    username_hint: Option<&str>,
) {
    let dialog = adw::Dialog::builder()
        .title(format!("Connect {service_name}"))
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

    if let Some(server) = server_hint {
        server_row.set_text(server);
    }
    if let Some(username) = username_hint {
        username_row.set_text(username);
    }

    let group = adw::PreferencesGroup::builder()
        .title("Connection details")
        .description(if service_name == "Other calendar server" {
            "Advanced: use the CalDAV address supplied by your calendar provider"
        } else {
            "Your password is saved securely in the system keyring"
        })
        .build();
    let has_fixed_server = server_hint.is_some();
    if !has_fixed_server {
        group.add(&server_row);
    }
    group.add(&username_row);
    group.add(&password_row);
    if !has_fixed_server {
        group.add(&http_row);
    }

    let note = gtk::Label::new(Some(
        "Many calendar services require an app password from their security \
         settings instead of your normal sign-in password.",
    ));
    // Capped for the same reason as the iCloud dialog: wrapping alone still
    // lets a long line widen the dialog. This one now carries server error
    // text, which can be arbitrarily long.
    const PROSE_WIDTH_CHARS: i32 = 44;

    note.set_wrap(true);
    note.set_max_width_chars(PROSE_WIDTH_CHARS);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");

    let error_label = gtk::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.set_max_width_chars(PROSE_WIDTH_CHARS);
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
        #[weak]
        dialog,
        #[weak]
        error_label,
        #[strong]
        connect_button,
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
            error_label.set_visible(false);
            add_caldav_account(
                &ui,
                &dialog,
                &error_label,
                &connect_button,
                server_url,
                username,
                password,
            );
        }
    ));

    dialog.present(Some(&ui.carousel));
}

/// Connects a generic CalDAV account, keeping `dialog` open until it succeeds.
/// Same reasoning as [`add_icloud_account`]: a rejected password should cost
/// one edit, not a full retype of server, username, and password.
///
/// Unlike iCloud there's no format to normalize against — every provider
/// issues its own shape — so the password is passed through untouched rather
/// than trimmed, since a server password may legitimately end in a space.
fn add_caldav_account(
    ui: &Rc<Ui>,
    dialog: &adw::Dialog,
    error_label: &gtk::Label,
    connect_button: &gtk::Button,
    server_url: String,
    username: String,
    password: String,
) {
    connect_button.set_sensitive(false);
    connect_button.set_label("Connecting…");

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
            // Secret before row — see the Google flow.
            icloud::credentials::save_app_password(&token_key, &credentials.password)
                .map_err(|e| e.to_string())?;
            let account_id = store
                .upsert_caldav_account(&username, &server_url, &display_name, &token_key)
                .map_err(|e| e.to_string())?;
            let outcome = caldav::sync_account(&credentials, &store, account_id);
            record_sync_result(&store, account_id, &display_name, &outcome);
            Ok(CaldavAddResult {
                display_name,
                outcome,
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
            dialog,
            #[strong]
            error_label,
            #[strong]
            connect_button,
            move || {
                let restore = || {
                    connect_button.set_label("Connect");
                    connect_button.set_sensitive(true);
                };
                match rx.try_recv() {
                    Ok(Ok(result)) => {
                        ui.toast_overlay
                            .add_toast(adw::Toast::new(&glib::markup_escape_text(&add_summary(
                                &result.outcome,
                                &result.display_name,
                                "calendar",
                            ))));
                        restore();
                        dialog.close();
                        ui.reset_calendar_sidebar();
                        ui.reset();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(error)) => {
                        error_label.set_label(first_line(&error));
                        error_label.set_visible(true);
                        restore();
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        error_label
                            .set_label("The connection attempt stopped unexpectedly. Try again.");
                        error_label.set_visible(true);
                        restore();
                        glib::ControlFlow::Break
                    }
                }
            }
        ),
    );
}

fn sync_caldav_accounts(ui: &Rc<Ui>, quiet: bool) {
    run_account_sync(ui, quiet, provider::CALDAV, || {
        let store = Store::open().map_err(|e| e.to_string())?;
        let accounts = store.caldav_accounts().map_err(|e| e.to_string())?;
        if accounts.is_empty() {
            return Err(provider::CALDAV.none_connected());
        }

        let account_count = accounts.len();
        let outcome = sync::sync_accounts(
            &accounts,
            |account| account.label(),
            |account| {
                sync_account_with_health(&store, account, || {
                    let base_url = account
                        .server_url
                        .clone()
                        .ok_or("no server address — remove and re-add the account")?;
                    let password = icloud::credentials::app_password(&account.token_key)
                        .map_err(|e| e.to_string())?
                        .ok_or("no saved password — open Accounts and choose Update password")?;
                    let credentials = caldav::Credentials {
                        base_url,
                        username: account.provider_account_id.clone(),
                        password,
                    };
                    caldav::sync_account(&credentials, &store, account.id)
                })
            },
        );
        Ok((account_count, outcome))
    });
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

    #[test]
    fn a_second_sync_of_one_provider_is_refused_while_the_first_is_in_flight() {
        let activity = AccountActivity::default();

        assert!(activity.start_sync(provider::GOOGLE));
        assert!(activity.is_syncing(provider::GOOGLE));
        assert!(!activity.start_sync(provider::GOOGLE));

        activity.finish_sync(provider::GOOGLE);
        assert!(!activity.is_syncing(provider::GOOGLE));
        assert!(activity.start_sync(provider::GOOGLE));
    }

    #[test]
    fn one_provider_syncing_leaves_the_others_free_to_start() {
        let activity = AccountActivity::default();

        assert!(activity.start_sync(provider::GOOGLE));
        assert!(activity.start_sync(provider::ICLOUD));

        activity.finish_sync(provider::GOOGLE);
        assert!(!activity.is_syncing(provider::GOOGLE));
        assert!(activity.is_syncing(provider::ICLOUD));
    }

    #[test]
    fn the_refresh_control_stays_busy_until_the_last_sync_finishes() {
        let activity = AccountActivity::default();
        assert!(!activity.any_sync_in_flight());

        activity.start_sync(provider::GOOGLE);
        activity.start_sync(provider::CALDAV);
        activity.finish_sync(provider::GOOGLE);
        assert!(activity.any_sync_in_flight());

        activity.finish_sync(provider::CALDAV);
        assert!(!activity.any_sync_in_flight());
    }

    #[test]
    fn a_second_interactive_sign_in_is_refused_while_one_is_open() {
        let activity = AccountActivity::default();

        assert!(activity.start_sign_in());
        assert!(!activity.start_sign_in());

        activity.finish_sign_in();
        assert!(activity.start_sign_in());
    }
    use chrono::TimeZone;

    const CTRL: gdk::ModifierType = gdk::ModifierType::CONTROL_MASK;
    const NONE: gdk::ModifierType = gdk::ModifierType::empty();

    #[test]
    fn ctrl_and_a_digit_switches_view_in_the_order_the_toggles_read() {
        assert_eq!(
            key_command(gdk::Key::_1, CTRL),
            Some(KeyCommand::ViewYear),
            "Ctrl+1 should pick the leftmost toggle"
        );
        assert_eq!(key_command(gdk::Key::_2, CTRL), Some(KeyCommand::ViewMonth));
        assert_eq!(key_command(gdk::Key::_3, CTRL), Some(KeyCommand::ViewWeek));
        assert_eq!(key_command(gdk::Key::_4, CTRL), Some(KeyCommand::ViewDay));
    }

    #[test]
    fn the_keypad_digits_work_too() {
        assert_eq!(
            key_command(gdk::Key::KP_1, CTRL),
            Some(KeyCommand::ViewYear)
        );
        assert_eq!(
            key_command(gdk::Key::KP_2, CTRL),
            Some(KeyCommand::ViewMonth)
        );
        assert_eq!(
            key_command(gdk::Key::KP_3, CTRL),
            Some(KeyCommand::ViewWeek)
        );
        assert_eq!(key_command(gdk::Key::KP_4, CTRL), Some(KeyCommand::ViewDay));
    }

    #[test]
    fn ctrl_f_opens_search() {
        assert_eq!(key_command(gdk::Key::f, CTRL), Some(KeyCommand::Search));
    }

    #[test]
    fn ctrl_navigates_and_creates() {
        assert_eq!(key_command(gdk::Key::t, CTRL), Some(KeyCommand::Today));
        assert_eq!(key_command(gdk::Key::n, CTRL), Some(KeyCommand::NewEvent));
        assert_eq!(
            key_command(gdk::Key::Left, CTRL),
            Some(KeyCommand::Previous)
        );
        assert_eq!(key_command(gdk::Key::Right, CTRL), Some(KeyCommand::Next));
    }

    #[test]
    fn an_unmodified_key_is_left_for_whatever_has_focus() {
        // The controller lives on the window, so anything claimed here is
        // taken away from an event title being typed into.
        for key in [
            gdk::Key::_1,
            gdk::Key::t,
            gdk::Key::n,
            gdk::Key::Left,
            gdk::Key::Right,
        ] {
            assert_eq!(
                key_command(key, NONE),
                None,
                "an unmodified key must reach the focused widget"
            );
        }
    }

    #[test]
    fn ctrl_z_undoes_and_the_two_usual_redo_chords_redo() {
        assert_eq!(key_command(gdk::Key::z, CTRL), Some(KeyCommand::Undo));
        assert_eq!(
            key_command(gdk::Key::Z, CTRL | gdk::ModifierType::SHIFT_MASK),
            Some(KeyCommand::Redo),
            "Ctrl+Shift+Z is the chord people arrive with"
        );
        assert_eq!(
            key_command(gdk::Key::y, CTRL),
            Some(KeyCommand::Redo),
            "and Ctrl+Y is the other one"
        );
    }

    #[test]
    fn caps_lock_does_not_turn_an_undo_into_a_redo() {
        // Lock produces the uppercase keyval with no Shift held, so reading the
        // case instead of the modifier would let a stuck lock key silently
        // reverse the command.
        assert_eq!(
            key_command(gdk::Key::Z, CTRL | gdk::ModifierType::LOCK_MASK),
            Some(KeyCommand::Undo)
        );
    }

    #[test]
    fn a_chord_carrying_alt_or_super_is_not_ours() {
        for extra in [gdk::ModifierType::ALT_MASK, gdk::ModifierType::SUPER_MASK] {
            assert_eq!(
                key_command(gdk::Key::_1, CTRL | extra),
                None,
                "Ctrl+Alt+1 and Ctrl+Super+1 belong to someone else"
            );
        }
    }

    #[test]
    fn caps_lock_does_not_break_a_shortcut() {
        // Lock rides along on ordinary presses and says nothing about intent.
        assert_eq!(
            key_command(gdk::Key::_1, CTRL | gdk::ModifierType::LOCK_MASK),
            Some(KeyCommand::ViewYear),
            "a stuck lock key must not disable the shortcut"
        );
        assert_eq!(
            key_command(gdk::Key::T, CTRL | gdk::ModifierType::SHIFT_MASK),
            Some(KeyCommand::Today),
            "Shift produces the uppercase keyval; it's the same chord"
        );
    }

    #[test]
    fn an_unbound_key_is_ignored() {
        assert_eq!(key_command(gdk::Key::_9, CTRL), None);
        assert_eq!(key_command(gdk::Key::k, CTRL), None);
    }

    fn local_midnight(year: i32, month: u32, day: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("unambiguous local midnight")
    }

    fn local_at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("unambiguous local time")
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
            attendees: Vec::new(),
            recurrence: None,
            reminder_minutes: None,
        }
    }

    /// A drag from 09:00 to 11:00, with the row as it stands afterwards.
    fn drag_fixture() -> (EventDraft, EventDraft, Event) {
        let original = test_event(local_at(2026, 7, 9, 9), local_at(2026, 7, 9, 10), false).draft();
        let optimistic =
            test_event(local_at(2026, 7, 9, 11), local_at(2026, 7, 9, 12), false).draft();
        let current = test_event(local_at(2026, 7, 9, 11), local_at(2026, 7, 9, 12), false);
        (original, optimistic, current)
    }

    #[test]
    fn a_failed_drag_undoes_the_move_it_wrote() {
        let (original, optimistic, current) = drag_fixture();

        let undo = undo_drag(&current, &optimistic, &original)
            .expect("the row is still what this drag wrote, so it must be put back");

        assert_eq!(undo.start, original.start);
        assert_eq!(undo.end, original.end);
    }

    #[test]
    fn a_failed_drag_leaves_a_newer_move_alone() {
        // The user dragged the same event again and that move stuck. This
        // drag's stale failure must not restore the state from before both.
        let (original, optimistic, _) = drag_fixture();
        let newer = test_event(local_at(2026, 7, 9, 15), local_at(2026, 7, 9, 16), false);

        assert!(undo_drag(&newer, &optimistic, &original).is_none());
    }

    #[test]
    fn undoing_a_drag_keeps_edits_made_to_the_rest_of_the_event() {
        // Only the times are this drag's to put back; a title typed in the
        // meantime belongs to whoever typed it.
        let (original, optimistic, mut current) = drag_fixture();
        current.title = "Renamed".to_string();

        let undo = undo_drag(&current, &optimistic, &original).expect("the times are untouched");

        assert_eq!(undo.title, "Renamed");
        assert_eq!(undo.start, original.start);
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

    /// One frame the carousel could present to the settle loop.
    fn frame(width: i32, position: f64) -> SettleFrame {
        SettleFrame {
            owns_carousel: true,
            page_attached: true,
            width,
            position,
            scrolled: false,
        }
    }

    #[test]
    fn rebuild_settle_waits_while_the_carousel_has_no_geometry() {
        // Screen blanked/locked or allocation pending: scrolling now would
        // silently park the carousel on page 0, the previous period.
        assert_eq!(settle_action(frame(0, 0.0)), SettleAction::Wait);
    }

    #[test]
    fn rebuild_settle_scrolls_until_parked_on_the_middle_page() {
        assert_eq!(settle_action(frame(800, 0.0)), SettleAction::Scroll);
    }

    #[test]
    fn rebuild_settle_finishes_once_parked_on_the_middle_page() {
        let landed = SettleFrame {
            scrolled: true,
            ..frame(800, 1.0)
        };
        assert_eq!(settle_action(landed), SettleAction::Done);
    }

    #[test]
    fn rebuild_settle_abandons_a_page_replaced_by_a_newer_rebuild() {
        let detached = SettleFrame {
            page_attached: false,
            scrolled: true,
            ..frame(800, 1.0)
        };
        assert_eq!(settle_action(detached), SettleAction::Abandon);
    }

    #[test]
    fn rebuild_settle_abandons_once_a_newer_rebuild_owns_the_carousel() {
        // The page can still be attached when a newer rebuild takes over, so
        // attachment alone isn't enough to notice being superseded.
        let superseded = SettleFrame {
            owns_carousel: false,
            scrolled: true,
            ..frame(800, 1.0)
        };
        assert_eq!(settle_action(superseded), SettleAction::Abandon);
    }

    #[test]
    fn rebuild_settle_scrolls_before_it_trusts_a_middle_page_reading() {
        // A rebuild that finds the carousel already reading 1.0 — a stale
        // position left over from the rebuild it replaced — must still issue
        // its own scroll. Trusting the reading is how a rebuild declared
        // itself finished without ever centering, leaving the previous
        // period on screen.
        assert_eq!(settle_action(frame(800, 1.0)), SettleAction::Scroll);
    }

    /// Runs the settle state machine over a scripted sequence of frames the
    /// way the tick callback does, so the *ordering* is testable without a
    /// display. The bug this fix targets was never in one decision; it was in
    /// which decision ran against which carousel state, and in whose rebuild
    /// got to clear the guard.
    struct SettleLoop {
        sync: Rc<Cell<CarouselSync>>,
        generation: u64,
        scrolled: bool,
        scrolls: usize,
        actions: Vec<SettleAction>,
    }

    impl SettleLoop {
        /// Begins a rebuild on `sync` and returns its settle loop.
        fn begin(sync: &Rc<Cell<CarouselSync>>) -> Self {
            let mut state = sync.get();
            let generation = state.begin_rebuild();
            sync.set(state);
            Self {
                sync: sync.clone(),
                generation,
                scrolled: false,
                scrolls: 0,
                actions: Vec::new(),
            }
        }

        /// Feeds one frame in, mirroring the real callback's side effects.
        /// Returns false once the loop has stopped.
        fn tick(&mut self, page_attached: bool, width: i32, position: f64) -> bool {
            let action = settle_action(SettleFrame {
                owns_carousel: self.sync.get().owns(self.generation),
                page_attached,
                width,
                position,
                scrolled: self.scrolled,
            });
            self.actions.push(action);
            match action {
                SettleAction::Scroll => {
                    self.scrolls += 1;
                    self.scrolled = true;
                    true
                }
                SettleAction::Wait => true,
                SettleAction::Done => {
                    let mut state = self.sync.get();
                    state.mark_settled(self.generation);
                    self.sync.set(state);
                    false
                }
                SettleAction::Abandon => false,
            }
        }

        /// Feeds `count` identical frames in, stopping early if the loop does.
        fn tick_n(&mut self, count: usize, width: i32, position: f64) {
            for _ in 0..count {
                if !self.tick(true, width, position) {
                    return;
                }
            }
        }
    }

    /// A carousel that centers on the frame after it's told to, which is how
    /// a healthy one behaves: the scroll lands, then the next frame reads 1.0.
    fn run_healthy_rebuild(sync: &Rc<Cell<CarouselSync>>) -> SettleLoop {
        let mut loop_ = SettleLoop::begin(sync);
        loop_.tick(true, 800, 0.0);
        loop_.tick(true, 800, MIDDLE_PAGE);
        loop_
    }

    #[test]
    fn nothing_is_settled_before_the_first_rebuild_lands() {
        // A fresh window must not read `page-changed` as a swipe: the
        // carousel is sitting at position 0 with no rebuild behind it.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        assert!(!sync.get().is_settled());

        let mut first = SettleLoop::begin(&sync);
        assert!(!sync.get().is_settled());

        // Frame clock stalled through startup — still not settled.
        first.tick_n(10, 0, 0.0);
        assert!(!sync.get().is_settled());
        assert!(first.actions.iter().all(|a| *a == SettleAction::Wait));

        // Frames start flowing: scroll, then confirm.
        first.tick(true, 800, 0.0);
        first.tick(true, 800, MIDDLE_PAGE);
        assert!(sync.get().is_settled());
        assert_eq!(first.scrolls, 1);
    }

    #[test]
    fn a_stalled_frame_clock_never_settles_a_rebuild() {
        // The blanked-screen / mid-resume case: a wall-clock timer would fire
        // here and hand the guard to a carousel still parked a period back.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        let mut rebuild = SettleLoop::begin(&sync);

        rebuild.tick_n(30, 0, 0.0);

        assert!(!sync.get().is_settled());
        assert_eq!(rebuild.scrolls, 0);
    }

    #[test]
    fn a_rebuild_inheriting_a_centered_position_still_scrolls_before_settling() {
        // Back-to-back navigation leaves position at 1.0 from the previous
        // rebuild. The new one must prove it centered its *own* pages.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        run_healthy_rebuild(&sync);

        let mut second = SettleLoop::begin(&sync);
        assert!(!sync.get().is_settled());

        assert!(second.tick(true, 800, MIDDLE_PAGE));
        assert_eq!(second.actions, vec![SettleAction::Scroll]);
        assert!(!sync.get().is_settled());

        second.tick(true, 800, MIDDLE_PAGE);
        assert!(sync.get().is_settled());
        assert_eq!(second.scrolls, 1);
    }

    #[test]
    fn a_superseded_rebuild_cannot_settle_the_carousel_for_a_newer_one() {
        // Two swipes in quick succession. The first rebuild's loop is still
        // running when the second claims the carousel; if the stale loop can
        // clear the guard, the next `page-changed` is read as a fresh swipe
        // and the period moves a second time. This is the bug a single
        // "rebuilding" boolean could not express.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        let mut first = SettleLoop::begin(&sync);
        first.tick(true, 800, 0.0);

        let second = SettleLoop::begin(&sync);

        // The first loop keeps running — its page is even still attached —
        // but it must not vouch for the carousel.
        assert!(
            !first.tick(true, 800, MIDDLE_PAGE),
            "a superseded loop must stop"
        );
        assert_eq!(first.actions.last(), Some(&SettleAction::Abandon));
        assert!(
            !sync.get().is_settled(),
            "the newer rebuild has not landed, so nothing is settled"
        );
        assert!(sync.get().owns(second.generation));
    }

    #[test]
    fn a_deferred_backward_swipe_holds_the_guard_across_the_delay() {
        // A backward swipe waits ~180ms for the animation to finish before
        // recycling pages. The guard has to be claimed at `page-changed`, not
        // when the deferred work runs — otherwise the carousel counts as
        // settled for that whole window and a second `page-changed` from the
        // same swipe moves the period again.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        run_healthy_rebuild(&sync);
        assert!(sync.get().is_settled());

        // page-changed(0) — handler claims the carousel, then defers.
        let mut claimed = sync.get();
        claimed.begin_rebuild();
        sync.set(claimed);

        assert!(
            !sync.get().is_settled(),
            "a page-changed arriving during the defer must be ignored"
        );

        // The deferred advance claims again, recycles, and centers.
        let mut deferred = SettleLoop::begin(&sync);
        deferred.tick(true, 800, 0.0);
        deferred.tick(true, 800, MIDDLE_PAGE);

        assert!(sync.get().is_settled());
        assert_eq!(deferred.scrolls, 1);
    }

    #[test]
    fn a_late_settle_from_an_old_generation_is_ignored() {
        let mut sync = CarouselSync::default();
        let stale = sync.begin_rebuild();
        let current = sync.begin_rebuild();

        sync.mark_settled(stale);
        assert!(!sync.is_settled());

        sync.mark_settled(current);
        assert!(sync.is_settled());
    }

    #[test]
    fn a_new_rebuild_unsettles_the_carousel() {
        // Navigation must close the window in which `page-changed` is
        // trusted, immediately and synchronously.
        let sync = Rc::new(Cell::new(CarouselSync::default()));
        run_healthy_rebuild(&sync);
        assert!(sync.get().is_settled());

        let mut state = sync.get();
        state.begin_rebuild();
        sync.set(state);

        assert!(!sync.get().is_settled());
    }

    #[test]
    fn a_forward_step_drops_the_stale_previous_page() {
        // Swiping forward leaves [prev, cur, next] showing `next`. Only the now
        // two-periods-stale `prev` may be touched: rebuilding the page the user
        // is looking at is what makes a swipe flicker.
        assert_eq!(
            recycle_plan(1),
            RecyclePlan {
                drop_first: true,
                insert_first: false
            }
        );
    }

    #[test]
    fn a_backward_step_drops_the_stale_next_page() {
        assert_eq!(
            recycle_plan(-1),
            RecyclePlan {
                drop_first: false,
                insert_first: true
            }
        );
    }

    #[test]
    fn only_the_outer_pages_are_read_as_swipes() {
        assert_eq!(swipe_delta(0), Some(-1));
        assert_eq!(swipe_delta(2), Some(1));
    }

    #[test]
    fn landing_on_the_middle_page_is_our_own_centering_not_a_swipe() {
        assert_eq!(swipe_delta(1), None);
    }
}
