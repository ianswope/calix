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

use crate::event_dialog::{self, CreateTarget, RemoteEvent, TargetChoice};
use crate::store::{Attendee, Event, Store};
use crate::views::{
    EventSelection, PageActions, PasteAction, SlotSelection, month_view, week_view, year_view,
};
use crate::{event_popover, location_completion, search};
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use gtk::glib;
use gtk::prelude::*;
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

fn survivors(build: impl FnOnce() -> gtk::Widget) -> Vec<String> {
    let root = build();
    let watched = watch(&root);
    drop(root);
    drain();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for (name, weak) in watched {
        if weak.upgrade().is_some() {
            *by_type.entry(name).or_default() += 1;
        }
    }
    by_type
        .into_iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect()
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
        on_move: Rc::new(|_, _, _, _| {}),
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
    let store = Rc::new(Store::open_in_memory().expect("an in-memory database"));

    let day = NaiveDate::from_ymd_opt(2026, 9, 1).expect("a real date");
    let timed = event(1, at(2026, 9, 1, 9), at(2026, 9, 1, 10), false);
    let all_day = event(2, at(2026, 9, 2, 0), at(2026, 9, 3, 0), true);
    // Spans midnight, so the week grid draws it as two clipped blocks and
    // takes the "not this block's own edge" paths.
    let overnight = event(3, at(2026, 9, 3, 22), at(2026, 9, 4, 2), false);
    let week_events = vec![timed.clone(), all_day.clone(), overnight];
    let (invited, remote) = invitation(4, at(2026, 9, 1, 9));

    let scenarios: Vec<Scenario> = vec![
        ("month page", {
            let events = vec![timed.clone(), all_day.clone()];
            Box::new(move || month_view::build(day, &events, actions()))
        }),
        ("week page", {
            let events = week_events.clone();
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
            let events = vec![timed.clone()];
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
