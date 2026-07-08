use crate::date_util::month_grid;
use crate::store::Event;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime};
use gtk::prelude::*;
use std::rc::Rc;

const WEEKDAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MAX_CHIPS_PER_CELL: usize = 3;

/// Builds a full month-grid page (weekday header + 6x7 day cells) anchored
/// on any date within the target month. `events` should already be scoped
/// to (at least) the grid's visible range. Clicking empty cell space calls
/// `on_create` with 9am on that date; clicking an event chip calls
/// `on_edit` with that event.
pub fn build(
    anchor: NaiveDate,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.set_homogeneous(true);
    header.set_hexpand(true);
    for label in WEEKDAY_LABELS {
        let weekday_label = gtk::Label::new(Some(label));
        weekday_label.add_css_class("caption-heading");
        weekday_label.add_css_class("dim-label");
        weekday_label.set_margin_top(6);
        weekday_label.set_margin_bottom(6);
        header.append(&weekday_label);
    }
    root.append(&header);

    let grid = gtk::Grid::builder()
        .row_homogeneous(true)
        .column_homogeneous(true)
        .vexpand(true)
        .hexpand(true)
        .build();

    let today = Local::now().date_naive();
    let current_month = anchor.month();

    for (i, date) in month_grid(anchor).iter().enumerate() {
        let row = (i / 7) as i32;
        let col = (i % 7) as i32;
        let day_events: Vec<Event> = events
            .iter()
            .filter(|e| e.start.date_naive() <= *date && e.end.date_naive() >= *date)
            .cloned()
            .collect();
        let cell = day_cell(
            *date,
            current_month,
            today,
            day_events,
            on_create.clone(),
            on_edit.clone(),
        );
        grid.attach(&cell, col, row, 1, 1);
    }

    root.append(&grid);
    root.upcast()
}

fn day_cell(
    date: NaiveDate,
    current_month: u32,
    today: NaiveDate,
    day_events: Vec<Event>,
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
    cell.add_css_class("month-cell");

    let number_label = gtk::Label::new(Some(&date.day().to_string()));
    number_label.set_halign(gtk::Align::Start);
    number_label.set_margin_start(6);
    number_label.set_margin_top(4);
    number_label.add_css_class("day-number");
    if date.month() != current_month {
        number_label.add_css_class("dim-label");
    }
    if date == today {
        number_label.add_css_class("today-badge");
    }
    cell.append(&number_label);

    let shown = day_events.len().min(MAX_CHIPS_PER_CELL);
    for event in &day_events[..shown] {
        let chip = gtk::Button::builder()
            .label(event.title.as_str())
            .css_classes(["event-chip"])
            .build();
        let ev = event.clone();
        let on_edit = on_edit.clone();
        chip.connect_clicked(move |_| on_edit(ev.clone()));
        cell.append(&chip);
    }
    if day_events.len() > MAX_CHIPS_PER_CELL {
        let more = gtk::Label::new(Some(&format!("+{} more", day_events.len() - MAX_CHIPS_PER_CELL)));
        more.add_css_class("caption");
        more.add_css_class("dim-label");
        more.set_halign(gtk::Align::Start);
        more.set_margin_start(6);
        cell.append(&more);
    }

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    cell.append(&spacer);

    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        let start = date
            .and_time(NaiveTime::from_hms_opt(9, 0, 0).expect("9:00:00 is a valid time"))
            .and_local_timezone(Local)
            .single();
        if let Some(start) = start {
            on_create(start);
        }
    });
    cell.add_controller(click);

    cell.upcast()
}
