use crate::date_util::week_dates;
use crate::store::Event;
use crate::views::{
    add_new_event_menu,
    drag::{BlockPlacement, DragKind, TimedGrid, parse_drag_payload},
    event_occurs_on_day, event_widget,
};
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
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
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
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
) -> gtk::Widget {
    build_days(vec![day], events, on_create, on_edit, on_move, true)
}

fn build_days(
    days: Vec<NaiveDate>,
    events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
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
    let timed_grid = TimedGrid::new(&day_area, days.clone(), HOUR_ROW_HEIGHT, on_move.clone());
    for day in &days {
        let day_events: Vec<Event> = events
            .iter()
            .filter(|event| event_occurs_on_day(event, *day))
            .cloned()
            .collect();
        let col_index = day_column_index(&days, *day);
        let column = day_column(
            *day,
            today,
            &day_events,
            on_create.clone(),
            on_edit.clone(),
            on_move.clone(),
            &timed_grid,
            col_index as usize,
        );
        day_area.attach(&column, col_index, 0, 1, 1);
    }

    // Overlay a preview layer above the day columns so an in-flight drag can
    // paint where the event will land without disturbing the real blocks.
    let grid_overlay = gtk::Overlay::new();
    grid_overlay.set_hexpand(true);
    grid_overlay.set_child(Some(&day_area));
    grid_overlay.add_overlay(timed_grid.preview_layer());
    hour_grid.append(&grid_overlay);

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
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
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

            add_drop_target(&cell, *day, None, on_move.clone());
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

#[allow(clippy::too_many_arguments)]
fn day_column(
    day: NaiveDate,
    today: NaiveDate,
    day_events: &[Event],
    on_create: Rc<dyn Fn(DateTime<Local>)>,
    on_edit: Rc<dyn Fn(Event)>,
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
    timed_grid: &Rc<TimedGrid>,
    col_index: usize,
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
    add_drop_target(&overlay, day, Some(HOUR_ROW_HEIGHT), on_move);

    // Right-clicking empty grid space offers a new event at that spot,
    // snapped down to the quarter hour it lands in.
    add_new_event_menu(
        &overlay,
        move |_, y| {
            let quarter = ((y / HOUR_ROW_HEIGHT as f64) * 4.0)
                .floor()
                .clamp(0.0, 95.0) as u32;
            day.and_time(NaiveTime::from_hms_opt(quarter / 4, (quarter % 4) * 15, 0)?)
                .and_local_timezone(Local)
                .single()
        },
        on_create.clone(),
    );

    let layouts = timed_event_layouts(day, day_events);
    if !layouts.is_empty() {
        let lane_count = layouts
            .iter()
            .map(|layout| layout.lane + 1)
            .max()
            .unwrap_or(1);
        let event_layer = gtk::Grid::new();
        event_layer.set_hexpand(true);
        event_layer.set_column_homogeneous(true);
        event_layer.set_size_request(-1, 24 * HOUR_ROW_HEIGHT);
        event_layer.set_valign(gtk::Align::Start);
        event_layer.set_halign(gtk::Align::Fill);

        let lane_layers = (0..lane_count)
            .map(|lane| {
                let lane_background = gtk::Box::new(gtk::Orientation::Vertical, 0);
                lane_background.set_size_request(-1, 24 * HOUR_ROW_HEIGHT);
                let lane_layer = gtk::Overlay::new();
                lane_layer.set_child(Some(&lane_background));
                lane_layer.set_hexpand(true);
                event_layer.attach(&lane_layer, lane as i32, 0, 1, 1);
                lane_layer
            })
            .collect::<Vec<_>>();

        for layout in layouts {
            let event = layout.event;
            let top = (layout.start_hour * HOUR_ROW_HEIGHT as f64).round() as i32;
            let height = (((layout.end_hour - layout.start_hour) * HOUR_ROW_HEIGHT as f64).round()
                as i32)
                .max(MIN_EVENT_BLOCK_HEIGHT);

            // An event spanning midnight is clipped to this day's block; an
            // edge is only draggable when it's the event's own start/end.
            // Ending exactly at next midnight still counts as ending here —
            // that's the block's 24:00 bottom edge.
            let starts_here = event.start.date_naive() == day;
            let ends_here = event.end.date_naive() == day
                || (event.end.time() == NaiveTime::MIN
                    && day.succ_opt() == Some(event.end.date_naive()));

            let block = event_widget::timed_event_widget(
                event,
                "event-block",
                MIN_EVENT_BLOCK_HEIGHT,
                on_edit.clone(),
                timed_grid,
                &BlockPlacement {
                    col: col_index,
                    top_px: top as f64,
                    height_px: height as f64,
                    starts_here,
                    ends_here,
                },
            );
            block.set_valign(gtk::Align::Start);
            block.set_halign(gtk::Align::Fill);
            block.set_hexpand(true);
            block.set_margin_top(top);
            block.set_size_request(-1, height);
            block.set_margin_start(2);
            block.set_margin_end(2);
            lane_layers[layout.lane].add_overlay(&block);
        }

        overlay.add_overlay(&event_layer);
    }

    if day == today {
        add_now_indicator(&overlay);
    }

    overlay.upcast()
}

fn add_drop_target(
    widget: &impl IsA<gtk::Widget>,
    date: NaiveDate,
    hour_height: Option<i32>,
    on_move: Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>,
) {
    let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    drop.connect_drop(move |_, value, _, y| {
        let Ok(event_id) = value.get::<String>() else {
            return false;
        };
        let Some((kind, event_id)) = parse_drag_payload(&event_id) else {
            return false;
        };
        let time = hour_height.and_then(|hour_height| time_for_y(y, hour_height));
        on_move(kind, event_id, date, time);
        true
    });
    widget.add_controller(drop);
}

fn time_for_y(y: f64, hour_height: i32) -> Option<NaiveTime> {
    let slots = ((y / hour_height as f64) * 2.0).floor().clamp(0.0, 47.0) as u32;
    NaiveTime::from_hms_opt(slots / 2, (slots % 2) * 30, 0)
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
            })
        })
        .collect();
    layouts.sort_by(|left, right| {
        left.start_hour
            .total_cmp(&right.start_hour)
            .then_with(|| left.end_hour.total_cmp(&right.end_hour))
    });

    let mut lane_ends = Vec::new();
    for layout in &mut layouts {
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
    layouts
}

fn hour_fraction(datetime: DateTime<Local>) -> f64 {
    datetime.hour() as f64 + datetime.minute() as f64 / 60.0
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
            account_server_url: None,
        }
    }

    #[test]
    fn overlapping_events_are_split_into_lanes() {
        let events = vec![event(1, 9, 11), event(2, 9, 10), event(3, 10, 12)];
        let day = events[0].start.date_naive();

        let layouts = timed_event_layouts(day, &events);

        assert_eq!(layouts.len(), 3);
        assert_eq!(layouts[0].event.id, 2);
        assert_eq!(layouts[1].event.id, 1);
        assert_eq!(layouts[2].event.id, 3);
        assert_ne!(layouts[0].lane, layouts[1].lane);
        assert_eq!(layouts[0].lane, layouts[2].lane);
    }
}
