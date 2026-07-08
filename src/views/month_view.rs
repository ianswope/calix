use crate::date_util::month_grid;
use chrono::{Datelike, Local, NaiveDate};
use gtk::prelude::*;

const WEEKDAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Builds a full month-grid page (weekday header + 6x7 day cells) anchored
/// on any date within the target month.
pub fn build(anchor: NaiveDate) -> gtk::Widget {
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
        grid.attach(&day_cell(*date, current_month, today), col, row, 1, 1);
    }

    root.append(&grid);
    root.upcast()
}

fn day_cell(date: NaiveDate, current_month: u32, today: NaiveDate) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
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

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    cell.append(&spacer);

    cell.upcast()
}
