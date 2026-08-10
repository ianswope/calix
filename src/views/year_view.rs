//! The year page: twelve month thumbnails, each a compact day grid.
//!
//! The thumbnail is deliberately its own function rather than inline, because
//! the sidebar wants exactly this widget for date-jumping — building it once
//! here means the two can't drift into looking like different products.

use crate::date_util::{month_grid, year_months};
use crate::store::Event;
use crate::views::event_occurs_on_day;
use chrono::{Datelike, Local, NaiveDate};
use gtk::prelude::*;
use std::rc::Rc;

/// Weekday initials above each thumbnail, starting on the same day
/// `week_start` uses.
const WEEKDAY_INITIALS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];

/// One cell of a month thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MonthCell {
    pub date: NaiveDate,
    /// Whether the date belongs to the thumbnail's own month. A 42-cell grid
    /// always spills into the neighbouring months, and those days must not be
    /// drawn as if they were this month's.
    pub in_month: bool,
    /// Whether anything is scheduled that day.
    pub busy: bool,
}

/// The 42 cells of `month`'s thumbnail, given the events for that year.
///
/// `busy` is computed against the whole event list rather than a pre-filtered
/// one so a caller can't accidentally mark a day quiet by having filtered it
/// out first — the grid spills into adjacent months, and those days still have
/// events.
pub(crate) fn month_cells(month: NaiveDate, events: &[Event]) -> [MonthCell; 42] {
    let grid = month_grid(month);
    std::array::from_fn(|i| {
        let date = grid[i];
        MonthCell {
            date,
            in_month: date.month() == month.month() && date.year() == month.year(),
            busy: events.iter().any(|event| event_occurs_on_day(event, date)),
        }
    })
}

/// Builds the year page for the year containing `anchor`.
pub fn build(anchor: NaiveDate, events: &[Event], on_pick: Rc<dyn Fn(NaiveDate)>) -> gtk::Widget {
    let grid = gtk::Grid::new();
    grid.set_row_spacing(18);
    grid.set_column_spacing(18);
    grid.set_margin_top(18);
    grid.set_margin_bottom(18);
    grid.set_margin_start(18);
    grid.set_margin_end(18);
    grid.set_row_homogeneous(true);
    grid.set_column_homogeneous(true);
    grid.set_hexpand(true);
    grid.set_vexpand(true);

    let today = Local::now().date_naive();
    for (index, month) in year_months(anchor).into_iter().enumerate() {
        let thumbnail = month_thumbnail(month, events, today, on_pick.clone());
        grid.attach(&thumbnail, index as i32 % 4, index as i32 / 4, 1, 1);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .build();
    scrolled.upcast()
}

/// One month as a compact, clickable day grid.
pub(crate) fn month_thumbnail(
    month: NaiveDate,
    events: &[Event],
    today: NaiveDate,
    on_pick: Rc<dyn Fn(NaiveDate)>,
) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.add_css_class("year-month");

    let name = gtk::Label::new(Some(&month.format("%B").to_string()));
    name.add_css_class("heading");
    name.set_xalign(0.0);
    column.append(&name);

    let days = gtk::Grid::new();
    days.set_row_homogeneous(true);
    days.set_column_homogeneous(true);

    for (index, initial) in WEEKDAY_INITIALS.iter().enumerate() {
        let label = gtk::Label::new(Some(initial));
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        days.attach(&label, index as i32, 0, 1, 1);
    }

    for (index, cell) in month_cells(month, events).into_iter().enumerate() {
        // Days from the neighbouring months keep their slot so the weeks stay
        // aligned, but are left blank — a thumbnail is read as a shape, and
        // spill days blur its edges.
        if !cell.in_month {
            continue;
        }
        let label = gtk::Label::new(Some(&cell.date.day().to_string()));
        label.add_css_class("caption");
        if cell.date == today {
            label.add_css_class("today-badge");
        } else if cell.busy {
            // Busy days are weighted rather than dotted: at this size a dot is
            // indistinguishable from a rendering artifact.
            label.add_css_class("year-day-busy");
        }

        let button = gtk::Button::builder().css_classes(["flat"]).build();
        button.set_child(Some(&label));
        button.set_tooltip_text(Some(&cell.date.format("%A, %B %-d").to_string()));
        let date = cell.date;
        let on_pick = on_pick.clone();
        button.connect_clicked(move |_| on_pick(date));

        days.attach(
            &button,
            index as i32 % 7,
            // Row 0 holds the weekday initials.
            index as i32 / 7 + 1,
            1,
            1,
        );
    }

    column.append(&days);
    column.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn d(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("a real date")
    }

    fn event_on(date: NaiveDate) -> Event {
        let start = Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 9, 0, 0)
            .single()
            .expect("an unambiguous local time");
        Event {
            id: 1,
            calendar_id: 1,
            calendar_name: "Test".to_string(),
            calendar_color: "#3584e4".to_string(),
            account_provider: None,
            account_provider_id: None,
            account_token_key: None,
            google_calendar_id: None,
            title: "Busy".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
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

    #[test]
    fn a_thumbnail_marks_only_its_own_months_days_as_in_month() {
        // August 2026 starts on a Saturday, so the grid opens with July days.
        let cells = month_cells(d(2026, 8, 1), &[]);
        let owned: Vec<NaiveDate> = cells
            .iter()
            .filter(|cell| cell.in_month)
            .map(|cell| cell.date)
            .collect();
        assert_eq!(owned.len(), 31, "August has 31 days");
        assert_eq!(owned[0], d(2026, 8, 1));
        assert_eq!(owned[30], d(2026, 8, 31));
        assert!(
            cells.iter().any(|cell| !cell.in_month),
            "a 42-cell grid always spills into its neighbours"
        );
    }

    #[test]
    fn a_day_with_an_event_is_marked_busy() {
        let cells = month_cells(d(2026, 8, 1), &[event_on(d(2026, 8, 12))]);
        let busy: Vec<NaiveDate> = cells
            .iter()
            .filter(|cell| cell.busy)
            .map(|cell| cell.date)
            .collect();
        assert_eq!(busy, vec![d(2026, 8, 12)]);
    }

    #[test]
    fn a_spill_day_still_reports_its_own_events() {
        // A July event showing in August's leading cells must not be reported
        // as a quiet day just because it isn't August's.
        let cells = month_cells(d(2026, 8, 1), &[event_on(d(2026, 7, 30))]);
        let spill = cells
            .iter()
            .find(|cell| cell.date == d(2026, 7, 30))
            .expect("July 30 is in August's grid");
        assert!(!spill.in_month);
        assert!(spill.busy, "the day has an event regardless of whose it is");
    }

    #[test]
    fn every_month_of_the_year_produces_a_full_grid() {
        for month in year_months(d(2026, 6, 15)) {
            let cells = month_cells(month, &[]);
            assert_eq!(cells.len(), 42);
            let owned = cells.iter().filter(|cell| cell.in_month).count();
            assert!(
                (28..=31).contains(&owned),
                "{month} claimed {owned} days of its own"
            );
        }
    }
}
