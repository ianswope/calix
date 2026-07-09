use crate::date_util::week_dates;
use crate::store::Event;
use crate::views::{event_occurs_on_day, event_widget};
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
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
) -> gtk::Widget {
    build_days(
        week_dates(anchor).to_vec(),
        events,
        on_create,
        on_edit,
        on_move,
        true,
    )
}

pub fn build_day(
    day: NaiveDate,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
) -> gtk::Widget {
    build_days(vec![day], events, on_create, on_edit, on_move, true)
}

fn build_days(
    days: Vec<NaiveDate>,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
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
        on_move.clone(),
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
        let column = day_column(
            *day,
            today,
            &day_events,
            on_create.clone(),
            on_edit.clone(),
            on_move.clone(),
        );
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
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
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

            add_drop_target(&cell, *day, on_move.clone());
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
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
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
    add_drop_target(&overlay, day, on_move);

    for layout in timed_event_layouts(day, day_events) {
        let event = layout.event;
        let start_h = layout.start_hour;
        let end_h = layout.end_hour;
        let top = (start_h * HOUR_ROW_HEIGHT as f64).round() as i32;
        let height = (((end_h - start_h) * HOUR_ROW_HEIGHT as f64).round() as i32)
            .max(MIN_EVENT_BLOCK_HEIGHT);

        let block = event_widget::event_button(event, "event-block", MIN_EVENT_BLOCK_HEIGHT);
        block.set_valign(gtk::Align::Start);
        block.set_halign(gtk::Align::Start);
        block.set_hexpand(false);
        block.set_margin_top(top);
        block.set_size_request(-1, height);

        let ev = event.clone();
        let on_edit = on_edit.clone();
        block.connect_clicked(move |_| on_edit(ev.clone()));

        overlay.add_overlay(&block);
        position_event_block(&overlay, &block, layout.lane, layout.lane_count, height);
    }

    if day == today {
        add_now_indicator(&overlay);
    }

    overlay.upcast()
}

fn add_drop_target(
    widget: &impl IsA<gtk::Widget>,
    date: NaiveDate,
    on_move: Rc<dyn Fn(i64, NaiveDate)>,
) {
    let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    drop.connect_drop(move |_, value, _, _| {
        let Ok(event_id) = value.get::<String>() else {
            return false;
        };
        let Ok(event_id) = event_id.parse::<i64>() else {
            return false;
        };
        on_move(event_id, date);
        true
    });
    widget.add_controller(drop);
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

struct TimedEventLayout<'a> {
    event: &'a Event,
    start_hour: f64,
    end_hour: f64,
    lane: usize,
    lane_count: usize,
}

fn timed_event_layouts(day: NaiveDate, events: &[Event]) -> Vec<TimedEventLayout<'_>> {
    let mut layouts: Vec<TimedEventLayout<'_>> = events
        .iter()
        .filter(|event| !event.all_day)
        .filter_map(|event| {
            let start_hour = if event.start.date_naive() < day {
                0.0
            } else {
                hour_fraction(event.start)
            };
            let end_hour = if event.end.date_naive() > day {
                24.0
            } else {
                hour_fraction(event.end)
            };
            (end_hour > start_hour).then_some(TimedEventLayout {
                event,
                start_hour,
                end_hour,
                lane: 0,
                lane_count: 1,
            })
        })
        .collect();
    layouts.sort_by(|left, right| {
        left.start_hour
            .total_cmp(&right.start_hour)
            .then_with(|| left.end_hour.total_cmp(&right.end_hour))
    });

    let mut cluster_start = 0;
    while cluster_start < layouts.len() {
        let mut cluster_end = layouts[cluster_start].end_hour;
        let mut cluster_end_index = cluster_start + 1;
        while cluster_end_index < layouts.len()
            && layouts[cluster_end_index].start_hour < cluster_end
        {
            cluster_end = cluster_end.max(layouts[cluster_end_index].end_hour);
            cluster_end_index += 1;
        }

        let mut lane_ends = Vec::new();
        for layout in &mut layouts[cluster_start..cluster_end_index] {
            let lane = lane_ends
                .iter()
                .position(|end: &f64| *end <= layout.start_hour)
                .unwrap_or_else(|| {
                    lane_ends.push(0.0);
                    lane_ends.len() - 1
                });
            lane_ends[lane] = layout.end_hour;
            layout.lane = lane;
        }
        for layout in &mut layouts[cluster_start..cluster_end_index] {
            layout.lane_count = lane_ends.len();
        }
        cluster_start = cluster_end_index;
    }
    layouts
}

fn hour_fraction(datetime: DateTime<Local>) -> f64 {
    datetime.hour() as f64 + datetime.minute() as f64 / 60.0
}

fn position_event_block(
    overlay: &gtk::Overlay,
    block: &gtk::Button,
    lane: usize,
    lane_count: usize,
    height: i32,
) {
    let position = move |overlay: &gtk::Overlay, block: &gtk::Button| {
        let available = (overlay.width() - 4).max(1);
        let lane_width = (available / lane_count as i32).max(1);
        block.set_margin_start(2 + lane as i32 * lane_width);
        block.set_size_request((lane_width - 4).max(1), height);
    };
    position(overlay, block);

    let weak_overlay = overlay.downgrade();
    let weak_block = block.downgrade();
    overlay.connect_notify_local(Some("width"), move |_, _| {
        if let (Some(overlay), Some(block)) = (weak_overlay.upgrade(), weak_block.upgrade()) {
            position(&overlay, &block);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn event(id: i64, start_hour: u32, end_hour: u32) -> Event {
        let start = Local
            .with_ymd_and_hms(2026, 7, 9, start_hour, 0, 0)
            .single()
            .unwrap();
        let end = Local
            .with_ymd_and_hms(2026, 7, 9, end_hour, 0, 0)
            .single()
            .unwrap();
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
            all_day: false,
            location: None,
            notes: None,
            google_event_id: None,
            icloud_event_id: None,
        }
    }

    #[test]
    fn overlapping_events_get_separate_lanes() {
        let events = vec![event(1, 9, 11), event(2, 9, 10), event(3, 10, 12)];
        let day = events[0].start.date_naive();

        let layouts = timed_event_layouts(day, &events);

        assert_eq!(layouts.len(), 3);
        assert!(layouts.iter().all(|layout| layout.lane_count == 2));
        assert_ne!(layouts[0].lane, layouts[1].lane);
    }
}
