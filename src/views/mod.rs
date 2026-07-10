use crate::store::Event;
use chrono::{NaiveDate, NaiveTime};

pub(crate) mod drag;
mod event_widget;
pub mod month_view;
pub mod week_view;

/// Whether an event's half-open time range includes a calendar date.
pub(crate) fn event_occurs_on_day(event: &Event, day: NaiveDate) -> bool {
    let start = event.start.date_naive();
    let mut end = event.end.date_naive();
    if event.end.time() == NaiveTime::MIN && event.end > event.start {
        end -= chrono::Duration::days(1);
    }
    start <= day && day <= end
}
