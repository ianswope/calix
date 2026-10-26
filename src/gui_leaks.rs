//! Leak checks for the GTK layer.
//!
//! GTK4 never runs dispose on a widget just because it was unparented: the
//! parent's reference goes, and if anything else still holds one, the widget,
//! its whole subtree and everything its closures captured stay alive for the
//! rest of the process. A closure attached to a widget's own controller or
//! button that captures that widget strongly is exactly such a holder — and
//! the grid is rebuilt after every sync, edit and navigation, so one such cycle
//! per cell is a leak that grows for as long as Calix is open.
//!
//! Each scenario here builds a widget the way the app does, takes a weak
//! reference to every widget in the tree, drops the root, and reports what is
//! still alive. It needs a display, so it is ignored by default and run by
//! hand:
//!
//! ```sh
//! cargo test gui_leaks -- --ignored
//! ```
//!
//! One test function rather than one per scenario, because GTK refuses to be
//! initialized from a second thread and the harness gives every test its own.
//! That same function drives the new-event dialog: a second test could not
//! initialize GTK again.

use crate::event_dialog::{self, CreateTarget, RemoteEvent, TargetChoice};
use crate::store::{Attendee, Event, Store};
use crate::views::{
    EventSelection, PageActions, PasteAction, SlotSelection, month_view, week_view, year_view,
};
use crate::{event_popover, location_completion, search};
use adw::prelude::*;
use chrono::{DateTime, Local, NaiveDate, TimeZone, Weekday};
use gtk::glib;
use std::collections::BTreeMap;
use std::rc::Rc;

/// One build to check: what the report calls it, and the widget it produces.
type Scenario = (&'static str, Box<dyn FnOnce() -> gtk::Widget>);

/// Every widget in `root`'s tree — popovers parented to a widget included,
/// since GTK4 makes them children — held weakly, with its type name for the
/// report.
fn watch(root: &gtk::Widget) -> Vec<(String, glib::WeakRef<gtk::Widget>)> {
    let mut watched = vec![(root.type_().name().to_string(), root.downgrade())];
    let mut child = root.first_child();
    while let Some(widget) = child {
        watched.extend(watch(&widget));
        child = widget.next_sibling();
    }
    // AdwDialog keeps its content in the child property, and that widget is
    // not a GTK child until the dialog is presented. Without this, the leak
    // sweep never sees the rows.
    if let Some(content) = root
        .downcast_ref::<adw::Dialog>()
        .and_then(|dialog| dialog.child())
    {
        let already = watched
            .iter()
            .any(|(_, weak)| weak.upgrade().is_some_and(|widget| widget == content));
        if !already {
            watched.extend(watch(&content));
        }
    }
    watched
}

/// Builds a tree, drops it, and names the widgets still alive afterwards, as
/// `GtkBox ×42` style counts. Pending main-loop work is run first, so a source
/// that legitimately holds a widget until it fires isn't mistaken for a leak.
/// Runs whatever main-loop work is already queued, so a source that
/// legitimately holds a widget until it fires isn't mistaken for a leak.
pub(crate) fn drain() {
    let context = glib::MainContext::default();
    for _ in 0..100 {
        if !context.pending() {
            break;
        }
        context.iteration(false);
    }
}

fn alive(root: &gtk::Widget) -> Vec<gtk::Widget> {
    watch(root)
        .into_iter()
        .filter_map(|(_, weak)| weak.upgrade())
        .collect()
}

fn combo(widgets: &[gtk::Widget], title: &str) -> adw::ComboRow {
    widgets
        .iter()
        .find_map(|widget| {
            let row = widget.downcast_ref::<adw::ComboRow>()?;
            (row.title() == title).then(|| row.clone())
        })
        .unwrap_or_else(|| panic!("no combo row titled {title}"))
}

fn spin(widgets: &[gtk::Widget], title: &str) -> adw::SpinRow {
    widgets
        .iter()
        .find_map(|widget| {
            let row = widget.downcast_ref::<adw::SpinRow>()?;
            (row.title() == title).then(|| row.clone())
        })
        .unwrap_or_else(|| panic!("no spin row titled {title}"))
}

fn entry(widgets: &[gtk::Widget], title: &str) -> adw::EntryRow {
    widgets
        .iter()
        .find_map(|widget| {
            let row = widget.downcast_ref::<adw::EntryRow>()?;
            (row.title() == title).then(|| row.clone())
        })
        .unwrap_or_else(|| panic!("no entry row titled {title}"))
}

fn action(widgets: &[gtk::Widget], title: &str) -> adw::ActionRow {
    widgets
        .iter()
        .find_map(|widget| {
            let row = widget.downcast_ref::<adw::ActionRow>()?;
            (row.title() == title).then(|| row.clone())
        })
        .unwrap_or_else(|| panic!("no row titled {title}"))
}

fn expander(widgets: &[gtk::Widget], title: &str) -> adw::ExpanderRow {
    widgets
        .iter()
        .find_map(|widget| {
            let row = widget.downcast_ref::<adw::ExpanderRow>()?;
            (row.title() == title).then(|| row.clone())
        })
        .unwrap_or_else(|| panic!("no expander titled {title}"))
}

fn button_labeled(widgets: &[gtk::Widget], label: &str) -> gtk::Button {
    widgets
        .iter()
        .find_map(|widget| {
            if widget.type_().name() != "GtkButton" {
                return None;
            }
            let button = widget.downcast_ref::<gtk::Button>()?;
            (button.label().as_deref() == Some(label)).then(|| button.clone())
        })
        .unwrap_or_else(|| panic!("no button labeled {label}"))
}

fn error_label(widgets: &[gtk::Widget]) -> gtk::Label {
    widgets
        .iter()
        .find_map(|widget| {
            let label = widget.downcast_ref::<gtk::Label>()?;
            label.has_css_class("error").then(|| label.clone())
        })
        .expect("the dialog's error label")
}

/// Weekday toggles, in the order they are laid out. Alert checks have no label.
fn weekday_toggles(widgets: &[gtk::Widget]) -> Vec<gtk::CheckButton> {
    widgets
        .iter()
        .filter_map(|widget| {
            let button = widget.downcast_ref::<gtk::CheckButton>()?;
            button.label().is_some().then(|| button.clone())
        })
        .collect()
}

fn press_weekday(buttons: &[gtk::CheckButton], day: Weekday) {
    let name = crate::locale::weekday_abbreviation(day);
    buttons
        .iter()
        .find(|button| button.label().as_deref() == Some(name.as_str()))
        .unwrap_or_else(|| panic!("a {name} button"))
        .set_active(true);
}

fn september_days(store: &Store) -> Vec<NaiveDate> {
    store
        .events_between(at(2026, 9, 1, 0), at(2026, 10, 1, 0))
        .expect("September reads")
        .into_iter()
        .map(|event| event.start.date_naive())
        .collect()
}

/// Block until the save poll writes an event, or two seconds pass. The create
/// path waits 100ms on the main loop, and `iteration(true)` would sit forever
/// once that source is gone, so a watchdog wakes the loop either way.
fn wait_for_september(store: &Store) -> bool {
    let timed_out = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = timed_out.clone();
    let watchdog = glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        flag.set(true);
        glib::ControlFlow::Break
    });
    let context = glib::MainContext::default();
    let mut saved = false;
    while !timed_out.get() {
        context.iteration(true);
        if !september_days(store).is_empty() {
            saved = true;
            break;
        }
    }
    if saved {
        watchdog.remove();
    }
    saved
}

/// What the custom repeat controls promise, exercised on the dialog Save uses.
///
/// Monday is set first. The unset default is Sunday, and a button row that
/// ignored the first weekday would still match that default.
fn saving_a_custom_repeat_stores_the_rule_the_dialog_described() {
    crate::date_util::set_first_weekday(Weekday::Mon);
    assert_eq!(
        crate::date_util::first_weekday(),
        Weekday::Mon,
        "this check has to run before anything else chooses the first weekday"
    );

    let store = Rc::new(Store::open_in_memory().expect("an in-memory database"));
    let dialog = event_dialog::build(
        store.clone(),
        local_target(),
        None,
        at(2026, 9, 1, 9),
        None,
        |_| {},
        |_| {},
        None,
    );
    // Save closes the dialog. Closing one that was never presented is a
    // critical from libadwaita, so host it on a window for the click.
    let window = gtk::Window::new();
    dialog.present(Some(&window));
    let widgets = alive(dialog.upcast_ref());
    let repeat = combo(&widgets, "Repeat");
    let custom = expander(&widgets, "Custom repeat");
    assert!(
        !custom.is_visible(),
        "custom repeat stays hidden until chosen"
    );
    repeat.set_selected(crate::recurrence::Frequency::CUSTOM_PICKER_INDEX);
    assert!(custom.is_visible());
    assert!(custom.is_expanded());
    assert_eq!(custom.subtitle_lines(), 2);
    assert_eq!(custom.subtitle(), "Every 2 weeks on the event's weekday");

    let interval = spin(&widgets, "Repeat every (weeks)");
    assert_eq!(interval.value(), 2.0);
    let frequency = combo(&widgets, "Frequency");
    for (index, title) in [
        (0, "Repeat every (days)"),
        (1, "Repeat every (weeks)"),
        (2, "Repeat every (months)"),
        (3, "Repeat every (years)"),
    ] {
        frequency.set_selected(index);
        assert_eq!(interval.title(), title);
    }
    frequency.set_selected(1);

    let days = action(&widgets, "On these days");
    assert!(days.is_visible());
    assert_eq!(
        days.subtitle().as_deref(),
        Some("Leave empty to use the event's weekday")
    );
    let toggles = weekday_toggles(&widgets);
    let expected: Vec<String> = crate::date_util::weekdays_from(Weekday::Mon)
        .into_iter()
        .map(crate::locale::weekday_abbreviation)
        .collect();
    let labels: Vec<String> = toggles
        .iter()
        .map(|button| button.label().expect("a labeled toggle").to_string())
        .collect();
    assert_eq!(labels, expected, "weekday buttons follow the first weekday");

    let ends = combo(&widgets, "Ends");
    let count = spin(&widgets, "Number of events");
    let until = entry(&widgets, "Stop on (YYYY-MM-DD)");
    let week = combo(&widgets, "Week of the month");
    assert_eq!(ends.selected(), 0);
    assert!(!count.is_visible());
    assert!(!until.is_visible());
    assert!(!week.is_visible());
    ends.set_selected(1);
    assert!(count.is_visible());
    assert!(!until.is_visible());
    assert_eq!(
        custom.subtitle(),
        "Every 2 weeks on the event's weekday, 10 times"
    );
    ends.set_selected(0);

    frequency.set_selected(2);
    week.set_selected(1);
    assert!(week.is_visible());
    assert_eq!(days.subtitle().as_deref(), Some("Choose one or more"));
    assert_eq!(custom.subtitle(), "Choose at least one weekday");
    entry(&widgets, "Title").set_text("Fortnightly");
    let error = error_label(&widgets);
    button_labeled(&widgets, "Save").emit_clicked();
    assert_eq!(error.label(), "Choose at least one weekday");
    assert!(
        september_days(&store).is_empty(),
        "a monthly weekday with no day chosen is not saved"
    );

    frequency.set_selected(1);
    assert_eq!(
        days.subtitle().as_deref(),
        Some("Leave empty to use the event's weekday")
    );
    ends.set_selected(2);
    assert!(!count.is_visible());
    assert!(until.is_visible());
    assert_eq!(custom.subtitle(), "Enter the stop date as YYYY-MM-DD");
    until.set_text("September 30");
    assert_eq!(custom.subtitle(), "Enter the stop date as YYYY-MM-DD");
    button_labeled(&widgets, "Save").emit_clicked();
    assert_eq!(error.label(), "Enter the stop date as YYYY-MM-DD");
    assert!(
        september_days(&store).is_empty(),
        "a stop date that is not YYYY-MM-DD is not saved"
    );

    until.set_text("2026-09-30");
    assert_eq!(
        custom.subtitle(),
        "Every 2 weeks on the event's weekday, until September 30, 2026"
    );
    press_weekday(&toggles, Weekday::Mon);
    press_weekday(&toggles, Weekday::Wed);
    assert_eq!(
        custom.subtitle(),
        "Every 2 weeks on Monday and Wednesday, until September 30, 2026"
    );
    button_labeled(&widgets, "Save").emit_clicked();
    assert!(
        wait_for_september(&store),
        "saving did not write the series ({})",
        error.label()
    );
    assert_eq!(
        september_days(&store),
        vec![
            NaiveDate::from_ymd_opt(2026, 9, 2).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        ]
    );
    let events = store
        .events_between(at(2026, 9, 1, 0), at(2026, 10, 1, 0))
        .expect("September reads");
    let rule = "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20260930";
    assert!(
        events.iter().all(|event| {
            event.title == "Fortnightly"
                && !event.all_day
                && event.start.format("%H:%M").to_string() == "09:00"
                && event
                    .recurrence
                    .map(crate::recurrence::Frequency::to_rrule)
                    .as_deref()
                    == Some(rule)
        }),
        "each occurrence keeps the saved series"
    );
    let master = store
        .event_by_id(events[0].id)
        .expect("the master reads")
        .expect("the master was stored");
    assert_eq!(
        master.start.date_naive(),
        NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        "the series stays anchored on the day that was open"
    );
    assert_eq!(
        master
            .recurrence
            .map(crate::recurrence::Frequency::to_rrule)
            .as_deref(),
        Some(rule)
    );

    let edit = event_dialog::build(
        store.clone(),
        Vec::new(),
        Some(master),
        at(2026, 9, 1, 9),
        None,
        |_| {},
        |_| {},
        None,
    );
    let edited = alive(edit.upcast_ref());
    assert_eq!(
        combo(&edited, "Repeat").selected(),
        crate::recurrence::Frequency::CUSTOM_PICKER_INDEX
    );
    let restored = expander(&edited, "Custom repeat");
    assert!(restored.is_visible());
    assert_eq!(
        restored.subtitle(),
        "Every 2 weeks on Monday and Wednesday, until September 30, 2026"
    );
    assert_eq!(combo(&edited, "Frequency").selected(), 1);
    assert_eq!(spin(&edited, "Repeat every (weeks)").value(), 2.0);
    assert_eq!(combo(&edited, "Ends").selected(), 2);
    assert_eq!(entry(&edited, "Stop on (YYYY-MM-DD)").text(), "2026-09-30");
    for button in weekday_toggles(&edited) {
        let name = button.label().expect("a labeled toggle");
        let on = name.as_str() == crate::locale::weekday_abbreviation(Weekday::Mon)
            || name.as_str() == crate::locale::weekday_abbreviation(Weekday::Wed);
        assert_eq!(
            button.is_active(),
            on,
            "{name} restored from the saved rule"
        );
    }

    let (invited, remote) = invitation(80, at(2026, 9, 1, 9));
    let start = invited.start;
    let synced = event_dialog::build(
        store.clone(),
        Vec::new(),
        Some(invited),
        start,
        None,
        |_| {},
        |_| {},
        Some(remote),
    );
    let synced_widgets = alive(synced.upcast_ref());
    assert!(
        synced_widgets.iter().any(|widget| {
            widget
                .downcast_ref::<adw::ComboRow>()
                .is_some_and(|row| row.title() == "Apply to")
        }),
        "the series scope row is part of this dialog"
    );
    assert!(
        synced_widgets.iter().all(|widget| {
            widget
                .downcast_ref::<adw::PreferencesRow>()
                .is_none_or(|row| {
                    let title = row.title();
                    title != "Repeat" && title != "Custom repeat"
                })
        }),
        "a synced event keeps the repeat rule its provider expanded"
    );

    drop((dialog, edit, synced, window));
    drain();
}

fn tally(watched: &[(String, glib::WeakRef<gtk::Widget>)]) -> Vec<String> {
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for (name, weak) in watched {
        if weak.upgrade().is_some() {
            *by_type.entry(name.clone()).or_default() += 1;
        }
    }
    by_type
        .into_iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect()
}

fn survivors(build: impl FnOnce() -> gtk::Widget) -> Vec<String> {
    let root = build();
    let watched = watch(&root);
    drop(root);
    drain();
    let mut alive = tally(&watched);
    if !alive.is_empty() {
        // The location field starts a lookup when an edit fills it in. The
        // popover stays alive until that debounce and request finish.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let context = glib::MainContext::default();
        while !alive.is_empty() && std::time::Instant::now() < deadline {
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let flag = fired.clone();
            let source = glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                flag.set(true);
                glib::ControlFlow::Break
            });
            context.iteration(true);
            if !fired.get() {
                source.remove();
            }
            drain();
            alive = tally(&watched);
        }
    }
    alive
}

fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("an unambiguous local time")
}

fn event(id: i64, start: DateTime<Local>, end: DateTime<Local>, all_day: bool) -> Event {
    Event {
        id,
        calendar_id: 1,
        calendar_name: "Local".to_string(),
        calendar_color: "#3584e4".to_string(),
        account_provider: None,
        account_provider_id: None,
        account_token_key: None,
        google_calendar_id: None,
        title: format!("Event {id}"),
        start,
        end,
        all_day,
        location: Some("Room 5".to_string()),
        notes: Some("https://meet.google.com/abc-defg-hij".to_string()),
        google_event_id: None,
        icloud_event_id: None,
        account_server_url: None,
        attendees: Vec::new(),
        recurrence: None,
        reminder_minutes: None,
        additional_reminders: Vec::new(),

        instance_of: None,
        original_start: None,
        cancelled: false,
    }
}

const GUEST: &str = "ian@example.com";

/// A CalDAV occurrence the signed-in user is invited to, so the inspector
/// grows its reply buttons and the dialog its "This event / All events" row.
fn invitation(id: i64, start: DateTime<Local>) -> (Event, RemoteEvent) {
    let mut event = event(id, start, start + chrono::Duration::hours(1), false);
    event.account_provider = Some("caldav".to_string());
    event.account_provider_id = Some(GUEST.to_string());
    event.account_token_key = Some("caldav-password:x".to_string());
    event.icloud_event_id = Some("/cal/series.ics#20260901T140000Z".to_string());
    event.account_server_url = Some("https://cal.example.com".to_string());
    event.attendees = vec![Attendee {
        email: GUEST.to_string(),
        name: Some("Ian".to_string()),
        status: Some("pending".to_string()),
        is_self: false,
    }];
    let remote = RemoteEvent::Caldav {
        base_url: "https://cal.example.com".to_string(),
        username: GUEST.to_string(),
        token_key: "caldav-password:x".to_string(),
        event_href: "/cal/series.ics#20260901T140000Z".to_string(),
    };
    (event, remote)
}

fn actions() -> PageActions {
    PageActions {
        on_create: Rc::new(|_, _| {}),
        on_edit: Rc::new(|_, _| {}),
        on_move: Rc::new(|_, _, _, _, _| {}),
        paste: PasteAction {
            ready: Rc::new(|| true),
            paste: Rc::new(|_| {}),
        },
        slots: SlotSelection::default(),
        events: EventSelection::default(),
    }
}

fn local_target() -> Vec<TargetChoice> {
    vec![TargetChoice {
        target: CreateTarget::Local {
            calendar_id: 1,
            name: "Local".to_string(),
        },
        visible: true,
    }]
}

#[test]
#[ignore = "needs a display: cargo test gui_leaks -- --ignored"]
fn dropping_a_widget_tree_frees_every_widget_in_it() {
    gtk::init().expect("a display to initialize GTK against");
    adw::init().expect("libadwaita to initialize");
    saving_a_custom_repeat_stores_the_rule_the_dialog_described();
    let store = Rc::new(Store::open_in_memory().expect("an in-memory database"));

    let day = NaiveDate::from_ymd_opt(2026, 9, 1).expect("a real date");
    let timed = event(1, at(2026, 9, 1, 9), at(2026, 9, 1, 10), false);
    // Exercise the real gesture after the builder has returned. Weak captures
    // must prevent cycles without letting the drag controller disappear early.
    let commits = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut drag_actions = actions();
    let recorded = commits.clone();
    drag_actions.on_move = Rc::new(move |kind, id, date, time, source_day| {
        recorded
            .borrow_mut()
            .push((kind, id, date, time, source_day));
    });
    let grid = week_view::build_hour_grid(&[day], std::slice::from_ref(&timed), drag_actions, 48);
    let block = watch(&grid)
        .into_iter()
        .filter_map(|(_, weak)| weak.upgrade())
        .find(|widget| widget.has_css_class("event-block"))
        .expect("a timed event block");
    let controllers = block.observe_controllers();
    let gesture = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i).and_downcast::<gtk::GestureDrag>())
        .next()
        .expect("the event's move gesture");
    gesture.emit_by_name::<()>("drag-update", &[&0.0_f64, &48.0_f64]);
    gesture.emit_by_name::<()>("drag-end", &[&0.0_f64, &48.0_f64]);
    assert_eq!(
        *commits.borrow(),
        vec![(
            crate::views::drag::DragKind::Move,
            timed.id,
            day,
            chrono::NaiveTime::from_hms_opt(10, 0, 0),
            Some(day)
        )],
        "dragging a built page must reschedule the event"
    );
    drop((gesture, controllers, block, grid));
    let all_day = event(2, at(2026, 9, 2, 0), at(2026, 9, 3, 0), true);
    // Spans midnight, so the week grid draws it as two clipped blocks and
    // takes the "not this block's own edge" paths.
    let overnight = event(3, at(2026, 9, 3, 22), at(2026, 9, 4, 2), false);
    let week_events = vec![timed.clone(), all_day.clone(), overnight];
    // A series is movable now: the week and month pages install its gesture
    // and drag source, which this leak sweep has to drop with the page.
    let mut series = event(5, at(2026, 9, 1, 11), at(2026, 9, 1, 12), false);
    series.recurrence = Some(crate::recurrence::Frequency::Weekly);
    let (invited, remote) = invitation(4, at(2026, 9, 1, 9));

    let scenarios: Vec<Scenario> = vec![
        ("month page", {
            let events = vec![timed.clone(), all_day.clone(), series.clone()];
            Box::new(move || month_view::build(day, &events, actions()))
        }),
        ("week page", {
            let mut events = week_events.clone();
            events.push(series.clone());
            Box::new(move || {
                week_view::build(
                    day,
                    &events,
                    actions(),
                    48,
                    week_view::InitialScroll::NowOrMorning,
                )
            })
        }),
        ("day page", {
            let events = vec![timed.clone(), series.clone()];
            Box::new(move || {
                week_view::build_day(
                    day,
                    &events,
                    actions(),
                    48,
                    week_view::InitialScroll::NowOrMorning,
                )
            })
        }),
        ("year page", {
            let events = vec![timed.clone()];
            Box::new(move || year_view::build(day, &events, Rc::new(|_| {})))
        }),
        ("event popover", {
            let (event, store) = (timed.clone(), store.clone());
            Box::new(move || {
                event_popover::build(
                    &event,
                    Rc::new(|_| {}),
                    store,
                    None,
                    Rc::new(|| {}),
                    Rc::new(|| {}),
                )
                .upcast()
            })
        }),
        ("event popover with reply buttons", {
            let (event, remote, store) = (invited.clone(), remote.clone(), store.clone());
            Box::new(move || {
                event_popover::build(
                    &event,
                    Rc::new(|_| {}),
                    store,
                    Some(remote),
                    Rc::new(|| {}),
                    Rc::new(|| {}),
                )
                .upcast()
            })
        }),
        ("search popover", {
            let store = store.clone();
            Box::new(move || search::build(store, Rc::new(|_| {})).0.upcast())
        }),
        ("new event dialog", {
            let store = store.clone();
            Box::new(move || {
                event_dialog::build(
                    store,
                    local_target(),
                    None,
                    at(2026, 9, 1, 9),
                    None,
                    |_| {},
                    |_| {},
                    None,
                )
                .upcast()
            })
        }),
        ("edit event dialog", {
            let (event, store) = (timed.clone(), store.clone());
            Box::new(move || {
                let start = event.start;
                event_dialog::build(
                    store,
                    Vec::new(),
                    Some(event),
                    start,
                    None,
                    |_| {},
                    |_| {},
                    None,
                )
                .upcast()
            })
        }),
        ("edit one local occurrence", {
            let (mut event, store) = (timed.clone(), store.clone());
            Box::new(move || {
                event.instance_of = Some(1);
                event.original_start = Some(event.start);
                let start = event.start;
                event_dialog::build(
                    store,
                    Vec::new(),
                    Some(event),
                    start,
                    None,
                    |_| {},
                    |_| {},
                    None,
                )
                .upcast()
            })
        }),
        ("edit series occurrence dialog", {
            let (event, remote, store) = (invited.clone(), remote.clone(), store.clone());
            Box::new(move || {
                let start = event.start;
                event_dialog::build(
                    store,
                    Vec::new(),
                    Some(event),
                    start,
                    None,
                    |_| {},
                    |_| {},
                    Some(remote),
                )
                .upcast()
            })
        }),
        ("location field", {
            let store = store.clone();
            Box::new(move || {
                let row = adw::EntryRow::new();
                location_completion::attach(&row, store);
                row.upcast()
            })
        }),
    ];

    // The cells and columns above avoid their cycle by reading the widget off
    // the gesture instead of capturing it, which is only sound if a controller
    // really does report the widget it was added to. Asserted here because a
    // click can't be synthesized in a test, so this is the part of that path
    // which can be checked rather than reasoned about.
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let gesture = gtk::GestureClick::new();
    cell.add_controller(gesture.clone());
    assert_eq!(
        gesture.widget().as_ref(),
        Some(cell.upcast_ref::<gtk::Widget>()),
        "a controller must report the widget it is attached to"
    );

    // Not a leak, but the one other popover failure that needs GTK to see: a
    // popover that can't fit beside its block never appears, taking its Edit
    // button with it. A Teams invite does it both ways — unbroken URLs set
    // the minimum width, and the whole boilerplate wrapped to the popover's
    // width ran taller than the window.
    let mut linked = timed.clone();
    linked.notes = Some(format!(
        "Need help?<https://teams.microsoft.com/l/meetup-join/{}>\n{}",
        "19%3ameeting_N2E2OWJlZTgtZmUxOS00MDUyLWJkYjAtYWNiYzBkYjEyNzFj".repeat(5),
        "Recording, transcription or screenshots are not permitted.\n".repeat(20)
    ));
    let inspector = event_popover::build(
        &linked,
        Rc::new(|_| {}),
        store.clone(),
        None,
        Rc::new(|| {}),
        Rc::new(|| {}),
    );
    let widgets: Vec<_> = watch(inspector.upcast_ref())
        .into_iter()
        .filter_map(|(_, weak)| weak.upgrade())
        .collect();
    assert!(
        widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk::LinkButton>()
                .is_some_and(|button| {
                    button.label().as_deref() == Some("Join meeting")
                        && button.uri().starts_with("https://teams.microsoft.com/")
                })
        }),
        "the inspector must expose its meeting link as an action"
    );
    assert!(
        widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.is_selectable() && label.label().contains("<a href="))
        }),
        "event details must contain selectable hyperlinks"
    );
    // The popover itself reports no size to its parent; its content is what
    // the surface has to fit.
    let content = inspector.child().expect("the inspector has content");
    let (min_width, natural_width, ..) = content.measure(gtk::Orientation::Horizontal, -1);
    assert!(
        min_width < 500,
        "a long URL in the notes must wrap, not widen the inspector to {min_width}px"
    );
    let (_, natural_height, ..) = content.measure(gtk::Orientation::Vertical, natural_width);
    assert!(
        natural_height < 600,
        "long notes must scroll, not stretch the inspector to {natural_height}px"
    );
    assert!(
        widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some("Edit"))
        }),
        "an ordinary event keeps its Edit button"
    );

    let mut subscribed = timed.clone();
    subscribed.account_provider = Some("webcal".to_string());
    let subscribed_inspector = event_popover::build(
        &subscribed,
        Rc::new(|_| {}),
        store.clone(),
        None,
        Rc::new(|| {}),
        Rc::new(|| {}),
    );
    let subscribed_widgets: Vec<_> = watch(subscribed_inspector.upcast_ref())
        .into_iter()
        .filter_map(|(_, weak)| weak.upgrade())
        .collect();
    assert!(
        subscribed_widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some("Copy"))
        }),
        "a subscribed event can still be copied"
    );
    assert!(
        subscribed_widgets.iter().all(|widget| {
            widget
                .downcast_ref::<gtk::Button>()
                .is_none_or(|button| button.label().as_deref() != Some("Edit"))
        }),
        "a subscribed event has no Edit button"
    );

    // The edit dialog's notes are edited where they can be read: an invite's
    // lines on lines, not run together in a one-line entry.
    let notes = event_dialog::NotesField::new();
    let one_line = notes.widget().measure(gtk::Orientation::Vertical, 400).1;
    let invite =
        "Microsoft Teams meeting\nJoin: https://teams.microsoft.com/meet/1\n\nPasscode: x\n"
            .repeat(3);
    notes.set_text(&invite);
    assert_eq!(notes.text(), invite, "notes must come back exactly as set");
    // A text view lays its lines out from the main loop, not on set_text.
    drain();
    let (_, natural_height, ..) = notes.widget().measure(gtk::Orientation::Vertical, 400);
    assert!(
        natural_height > one_line,
        "a dozen lines of notes must be taller than one ({natural_height}px vs {one_line}px)"
    );

    let mut leaks = Vec::new();
    for (name, build) in scenarios {
        let alive = survivors(build);
        if !alive.is_empty() {
            leaks.push(format!("{name}: {}", alive.join(", ")));
        }
    }

    // The window's own state is the other half of the problem: `Ui` holds the
    // carousel, the sidebar and the mini month, and each of those carries
    // handlers that hold the `Ui` back. Nothing frees either side, so in
    // background mode a closed-and-reopened window leaves the old `Ui` — and
    // its sync, alert and clock timers — running for the life of the process.
    if !crate::window::a_wired_ui_is_freed_with_its_widgets() {
        leaks.push("window state: the Ui outlived every widget it was wired to".to_string());
    }

    assert!(
        leaks.is_empty(),
        "still alive after being dropped:\n  {}",
        leaks.join("\n  ")
    );
}
