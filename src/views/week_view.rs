use crate::date_util::week_dates;
use crate::store::Event;
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
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);
    let today = Local::now().date_naive();
    let days = week_dates(anchor);

    root.append(&day_header_row(&days, today));

    let hour_grid = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hour_grid.set_hexpand(true);
    hour_grid.append(&gutter_column());
    for day in days {
        let day_events: Vec<Event> = events
            .iter()
            .filter(|e| e.start.date_naive() <= day && e.end.date_naive() >= day)
            .cloned()
            .collect();
        hour_grid.append(&day_column(
            day,
            today,
            &day_events,
            on_create.clone(),
            on_edit.clone(),
        ));
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&hour_grid)
        .build();

    // Land on a sensible starting scroll position (a couple hours before
    // now, or 8 AM for weeks that don't include today) once layout settles.
    let scroll_hour = if days.contains(&today) {
        today.and_time(Local::now().time()).time().hour().saturating_sub(2)
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

fn day_header_row(days: &[NaiveDate; 7], today: NaiveDate) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(true);

    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER_WIDTH, -1);
    row.append(&gutter);

    for day in days {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_hexpand(true);
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
        row.append(&col);
    }

    row.upcast()
}

fn gutter_column() -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.set_size_request(GUTTER_WIDTH, -1);
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
    overlay.set_child(Some(&col));

    if day == today {
        let now = Local::now().time();
        let offset = ((now.hour() as f64 + now.minute() as f64 / 60.0) / 24.0)
            * (24 * HOUR_ROW_HEIGHT) as f64;

        let now_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        now_line.add_css_class("now-line");
        now_line.set_valign(gtk::Align::Start);
        now_line.set_halign(gtk::Align::Fill);
        now_line.set_margin_top(offset as i32);
        now_line.set_size_request(-1, 2);
        overlay.add_overlay(&now_line);
    }

    for event in day_events {
        if event.start.date_naive() != day {
            continue;
        }
        let start_h = event.start.hour() as f64 + event.start.minute() as f64 / 60.0;
        let end_h = if event.end.date_naive() == day {
            (event.end.hour() as f64 + event.end.minute() as f64 / 60.0).max(start_h)
        } else {
            24.0
        };
        let top = (start_h * HOUR_ROW_HEIGHT as f64).round() as i32;
        let height =
            (((end_h - start_h) * HOUR_ROW_HEIGHT as f64).round() as i32).max(MIN_EVENT_BLOCK_HEIGHT);

        let block = gtk::Button::builder()
            .label(event.title.as_str())
            .css_classes(["event-block"])
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Fill)
            .build();
        block.set_margin_top(top);
        block.set_size_request(-1, height);
        block.set_margin_start(2);
        block.set_margin_end(2);

        let ev = event.clone();
        let on_edit = on_edit.clone();
        block.connect_clicked(move |_| on_edit(ev.clone()));

        overlay.add_overlay(&block);
    }

    overlay.upcast()
}

fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".to_string(),
        1..=11 => format!("{hour} AM"),
        12 => "12 PM".to_string(),
        _ => format!("{} PM", hour - 12),
    }
}
