use crate::date_util::week_dates;
use crate::store::Event;
use crate::views::event_widget;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, Timelike};
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;

const HOUR_ROW_HEIGHT: i32 = 48;
const GUTTER_WIDTH: i32 = 56;
const MIN_EVENT_BLOCK_HEIGHT: i32 = 20;

/// Builds a full week page: day-of-week header plus a scrollable 24-hour
/// grid, with a "now" indicator line on today's column if it's in view.
/// `events` should already be scoped to (at least) the visible week.
/// Clicking an empty hour slot calls `on_create` with that moment; clicking
/// an event block calls `on_edit` with that event.
pub fn build(
    anchor: NaiveDate,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
) -> gtk::Widget {
    build_days(
        week_dates(anchor).to_vec(),
        events,
        on_create,
        on_edit,
        true,
    )
}

pub fn build_day(
    day: NaiveDate,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
) -> gtk::Widget {
    build_days(vec![day], events, on_create, on_edit, true)
}

fn build_days(
    days: Vec<NaiveDate>,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
    scroll_to_today: bool,
) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);
    let today = Local::now().date_naive();
    let gutter_size_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);

    root.append(&day_header_row(&days, today, &gutter_size_group));
    root.append(&all_day_row(
        &days,
        events,
        on_edit.clone(),
        &gutter_size_group,
    ));

    let hour_grid = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hour_grid.set_hexpand(true);
    hour_grid.append(&gutter_column(&gutter_size_group));

    let day_area = day_area();
    for day in &days {
        let day_events: Vec<Event> = events
            .iter()
            .filter(|event| event_occurs_on_day(event, *day))
            .cloned()
            .collect();
        let column = day_column(*day, today, &day_events, on_create.clone(), on_edit.clone());
        day_area.attach(&column, day_column_index(&days, *day), 0, 1, 1);
    }
    hour_grid.append(&day_area);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .overlay_scrolling(true)
        .hexpand(true)
        .vexpand(true)
        .child(&hour_grid)
        .build();

    // Land on a sensible starting scroll position (a couple hours before
    // now, or 8 AM for weeks that don't include today) once layout settles.
    let scroll_hour = if scroll_to_today && days.contains(&today) {
        today
            .and_time(Local::now().time())
            .time()
            .hour()
            .saturating_sub(2)
    } else {
        8
    };
    glib::idle_add_local_once(glib::clone!(
        #[weak]
        scrolled,
        move || {
            scrolled
                .vadjustment()
                .set_value((scroll_hour * HOUR_ROW_HEIGHT as u32) as f64);
        }
    ));

    root.append(&scrolled);
    root.upcast()
}

fn day_header_row(
    days: &[NaiveDate],
    today: NaiveDate,
    gutter_size_group: &gtk::SizeGroup,
) -> gtk::Widget {
    row_with_days(gutter_size_group, |day_area| {
        for (i, day) in days.iter().enumerate() {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
            col.set_hexpand(true);
            col.set_size_request(1, -1);
            col.add_css_class("week-header-cell");

            let weekday = gtk::Label::new(Some(&day.format("%a").to_string()));
            weekday.add_css_class("caption-heading");
            weekday.add_css_class("dim-label");

            let number = gtk::Label::new(Some(&day.day().to_string()));
            number.add_css_class("title-3");
            if *day == today {
                number.add_css_class("today-badge");
            }

            col.append(&weekday);
            col.append(&number);
            day_area.attach(&col, i as i32, 0, 1, 1);
        }
    })
}

fn all_day_row(
    days: &[NaiveDate],
    events: &[Event],
    on_edit: Rc<dyn Fn(Event)>,
    gutter_size_group: &gtk::SizeGroup,
) -> gtk::Widget {
    let row = row_with_days(gutter_size_group, |day_area| {
        for (i, day) in days.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
            cell.set_hexpand(true);
            cell.set_size_request(1, -1);
            cell.add_css_class("all-day-cell");

            for event in events
                .iter()
                .filter(|event| event.all_day && event_occurs_on_day(event, *day))
            {
                let chip = event_widget::compact_event_button(event, "event-chip", 14);
                chip.add_css_class("all-day-event");
                chip.set_halign(gtk::Align::Fill);
                chip.set_hexpand(true);
                chip.set_size_request(1, -1);
                chip.set_margin_start(2);
                chip.set_margin_end(2);

                let ev = event.clone();
                let on_edit = on_edit.clone();
                chip.connect_clicked(move |_| on_edit(ev.clone()));
                cell.append(&chip);
            }

            day_area.attach(&cell, i as i32, 0, 1, 1);
        }
    });
    row.add_css_class("all-day-row");
    row
}

fn row_with_days(
    gutter_size_group: &gtk::SizeGroup,
    build_days: impl FnOnce(&gtk::Grid),
) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(true);
    row.set_homogeneous(false);

    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER_WIDTH, -1);
    gutter.add_css_class("week-gutter");
    gutter_size_group.add_widget(&gutter);
    row.append(&gutter);

    let day_area = day_area();
    build_days(&day_area);
    row.append(&day_area);
    row.upcast()
}

fn day_area() -> gtk::Grid {
    let grid = gtk::Grid::new();
    grid.set_hexpand(true);
    grid.set_column_homogeneous(true);
    grid
}

fn day_column_index(days: &[NaiveDate], day: NaiveDate) -> i32 {
    days.iter()
        .position(|date| *date == day)
        .expect("day belongs to the rendered range") as i32
}

fn gutter_column(gutter_size_group: &gtk::SizeGroup) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.set_size_request(GUTTER_WIDTH, -1);
    col.add_css_class("week-gutter");
    gutter_size_group.add_widget(&col);
    for hour in 0..24u32 {
        let label = gtk::Label::new(Some(&hour_label(hour)));
        label.set_size_request(-1, HOUR_ROW_HEIGHT);
        label.set_valign(gtk::Align::Start);
        label.set_halign(gtk::Align::End);
        label.set_margin_end(6);
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        col.append(&label);
    }
    col.upcast()
}

fn day_column(
    day: NaiveDate,
    today: NaiveDate,
    day_events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.set_hexpand(true);
    col.set_size_request(1, -1);
    if day == today {
        col.add_css_class("today-column");
    }

    for hour in 0..24u32 {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        cell.set_size_request(-1, HOUR_ROW_HEIGHT);
        cell.add_css_class("hour-cell");

        let click = gtk::GestureClick::new();
        let on_create = on_create.clone();
        click.connect_released(move |_, _, _, _| {
            let start = day
                .and_time(NaiveTime::from_hms_opt(hour, 0, 0).expect("hour is always 0..24"))
                .and_local_timezone(Local)
                .single();
            if let Some(start) = start {
                on_create(start);
            }
        });
        cell.add_controller(click);

        col.append(&cell);
    }

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("week-day-column");
    overlay.set_child(Some(&col));

    for event in day_events {
        if event.all_day || event.start.date_naive() != day {
            continue;
        }
        let start_h = event.start.hour() as f64 + event.start.minute() as f64 / 60.0;
        let end_h = if event.end.date_naive() == day {
            (event.end.hour() as f64 + event.end.minute() as f64 / 60.0).max(start_h)
        } else {
            24.0
        };
        let top = (start_h * HOUR_ROW_HEIGHT as f64).round() as i32;
        let height = (((end_h - start_h) * HOUR_ROW_HEIGHT as f64).round() as i32)
            .max(MIN_EVENT_BLOCK_HEIGHT);

        let block = event_widget::event_button(event, "event-block", MIN_EVENT_BLOCK_HEIGHT);
        block.set_valign(gtk::Align::Start);
        block.set_halign(gtk::Align::Fill);
        block.set_margin_top(top);
        block.set_size_request(-1, height);
        block.set_margin_start(2);
        block.set_margin_end(2);

        let ev = event.clone();
        let on_edit = on_edit.clone();
        block.connect_clicked(move |_| on_edit(ev.clone()));

        overlay.add_overlay(&block);
    }

    if day == today {
        add_now_indicator(&overlay);
    }

    overlay.upcast()
}

fn add_now_indicator(overlay: &gtk::Overlay) {
    let now = Local::now().time();
    let offset =
        ((now.hour() as f64 + now.minute() as f64 / 60.0) / 24.0) * (24 * HOUR_ROW_HEIGHT) as f64;

    let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    indicator.set_can_target(false);
    indicator.set_valign(gtk::Align::Start);
    indicator.set_halign(gtk::Align::Fill);
    indicator.set_margin_top(offset.round() as i32);
    indicator.set_size_request(-1, 8);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("now-dot");
    dot.set_valign(gtk::Align::Center);
    dot.set_size_request(8, 8);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("now-line");
    line.set_valign(gtk::Align::Center);
    line.set_hexpand(true);
    line.set_size_request(-1, 2);

    indicator.append(&dot);
    indicator.append(&line);
    overlay.add_overlay(&indicator);
}

fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".to_string(),
        1..=11 => format!("{hour} AM"),
        12 => "12 PM".to_string(),
        _ => format!("{} PM", hour - 12),
    }
}

fn event_occurs_on_day(event: &Event, day: NaiveDate) -> bool {
    let start = event.start.date_naive();
    let mut end = event.end.date_naive();
    if event.all_day && event.end.time() == NaiveTime::MIN && event.end > event.start {
        end -= chrono::Duration::days(1);
    }
    start <= day && end >= day
}
