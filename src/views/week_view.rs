use crate::date_util::week_dates;
use chrono::{Datelike, Local, NaiveDate, Timelike};
use gtk::glib;
use gtk::prelude::*;

const HOUR_ROW_HEIGHT: i32 = 48;
const GUTTER_WIDTH: i32 = 56;

/// Builds a full week page: day-of-week header plus a scrollable 24-hour
/// grid, with a "now" indicator line on today's column if it's in view.
pub fn build(anchor: NaiveDate) -> gtk::Widget {
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
        hour_grid.append(&day_column(day, today));
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

fn day_column(day: NaiveDate, today: NaiveDate) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.set_hexpand(true);

    for _ in 0..24 {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        cell.set_size_request(-1, HOUR_ROW_HEIGHT);
        cell.add_css_class("hour-cell");
        col.append(&cell);
    }

    if day == today {
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&col));

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

        overlay.upcast()
    } else {
        col.upcast()
    }
}

fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".to_string(),
        1..=11 => format!("{hour} AM"),
        12 => "12 PM".to_string(),
        _ => format!("{} PM", hour - 12),
    }
}
