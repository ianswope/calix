use chrono::{DateTime, Datelike, Local, Months, NaiveDate, NaiveTime, TimeZone, Weekday};

/// First day of the week, matching Apple/Google Calendar's US default.
const WEEK_START: Weekday = Weekday::Sun;

/// The start (WEEK_START) of the week containing `date`.
pub fn week_start(date: NaiveDate) -> NaiveDate {
    let days_since_start = date.weekday().days_since(WEEK_START);
    date - chrono::Duration::days(days_since_start as i64)
}

/// The 7 dates of the week containing `date`, starting on WEEK_START.
pub fn week_dates(date: NaiveDate) -> [NaiveDate; 7] {
    let start = week_start(date);
    std::array::from_fn(|i| start + chrono::Duration::days(i as i64))
}

/// The first day of the month containing `date`.
pub fn month_start(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("day 1 is always valid")
}

/// 42 dates (6 full weeks) covering the month containing `date`, including
/// leading/trailing days from adjacent months, starting on WEEK_START.
pub fn month_grid(date: NaiveDate) -> [NaiveDate; 42] {
    let grid_start = week_start(month_start(date));
    std::array::from_fn(|i| grid_start + chrono::Duration::days(i as i64))
}

/// Half-open [start, end) date range covering everything `month_grid` shows.
pub fn month_grid_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let grid = month_grid(date);
    (grid[0], grid[41] + chrono::Duration::days(1))
}

/// Half-open [start, end) date range covering the week containing `date`.
pub fn week_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let start = week_start(date);
    (start, start + chrono::Duration::days(7))
}

/// Half-open [start, end) date range covering a single day.
pub fn day_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    (date, date + chrono::Duration::days(1))
}

/// Shift `date` forward (delta > 0) or backward (delta < 0) by whole months,
/// clamping the day-of-month if the target month is shorter.
pub fn shift_months(date: NaiveDate, delta: i32) -> NaiveDate {
    if delta >= 0 {
        date.checked_add_months(Months::new(delta as u32))
    } else {
        date.checked_sub_months(Months::new((-delta) as u32))
    }
    .expect("date arithmetic stays within chrono's supported range")
}

/// Shift `date` forward/backward by whole weeks.
pub fn shift_weeks(date: NaiveDate, delta: i32) -> NaiveDate {
    date + chrono::Duration::weeks(delta as i64)
}

/// Shift `date` forward/backward by whole days.
pub fn shift_days(date: NaiveDate, delta: i32) -> NaiveDate {
    date + chrono::Duration::days(delta as i64)
}

/// The first instant of the calendar day `date` in timezone `tz`.
///
/// Almost every day starts at 00:00, but real timezone histories include days
/// whose midnight was skipped or repeated by a DST/offset transition — for
/// example São Paulo sprang forward at 00:00 on 2017-10-15 (so that day has no
/// midnight), and Pacific/Apia skipped all of 2011-12-30 for the date-line
/// change. Rather than panic (as `.single().expect(...)` would) or silently
/// drop the day (as `.single()?` would), resolve to the first wall-clock
/// instant the civil day actually reached.
pub fn day_start_in<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> DateTime<Tz> {
    let midnight = date.and_time(NaiveTime::MIN);
    if let Some(instant) = tz.from_local_datetime(&midnight).earliest() {
        return instant;
    }
    // Midnight itself was skipped: walk forward to the first minute this civil
    // day (or, for a wholly skipped date, the next one) actually reached.
    (1..=48 * 60)
        .find_map(|minutes| {
            tz.from_local_datetime(&(midnight + chrono::Duration::minutes(minutes)))
                .earliest()
        })
        .unwrap_or_else(|| tz.from_utc_datetime(&midnight))
}

/// [`day_start_in`] specialized to the system's local timezone — turning a
/// `NaiveDate` (as the calendar grids use) into the local `DateTime` that the
/// storage and sync layers work in.
pub fn local_day_start(date: NaiveDate) -> DateTime<Local> {
    day_start_in(&Local, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn week_start_lands_on_sunday() {
        // 2026-07-08 is a Wednesday.
        assert_eq!(week_start(d(2026, 7, 8)), d(2026, 7, 5));
        // Sunday itself should map to itself.
        assert_eq!(week_start(d(2026, 7, 5)), d(2026, 7, 5));
    }

    #[test]
    fn week_dates_spans_seven_consecutive_days() {
        let dates = week_dates(d(2026, 7, 8));
        assert_eq!(dates[0], d(2026, 7, 5));
        assert_eq!(dates[6], d(2026, 7, 11));
    }

    #[test]
    fn month_grid_covers_full_month_with_padding() {
        // July 2026: 1st is a Wednesday, 31 days.
        let grid = month_grid(d(2026, 7, 15));
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0], d(2026, 6, 28)); // leading days from June
        assert!(grid.contains(&d(2026, 7, 1)));
        assert!(grid.contains(&d(2026, 7, 31)));
        assert_eq!(grid[41], d(2026, 8, 8)); // trailing days into August
    }

    #[test]
    fn shift_months_clamps_short_months() {
        // Jan 31 + 1 month -> Feb has no 31st, chrono clamps to Feb 28/29.
        assert_eq!(shift_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(shift_months(d(2026, 3, 1), -1), d(2026, 2, 1));
    }

    #[test]
    fn shift_weeks_moves_by_seven_days() {
        assert_eq!(shift_weeks(d(2026, 7, 8), 1), d(2026, 7, 15));
        assert_eq!(shift_weeks(d(2026, 7, 8), -1), d(2026, 7, 1));
    }

    #[test]
    fn shift_days_moves_by_one_day() {
        assert_eq!(shift_days(d(2026, 7, 8), 1), d(2026, 7, 9));
        assert_eq!(shift_days(d(2026, 7, 8), -1), d(2026, 7, 7));
    }

    #[test]
    fn day_start_is_midnight_for_an_ordinary_day() {
        let tz = chrono_tz::America::New_York;
        assert_eq!(
            day_start_in(&tz, d(2026, 3, 10)).naive_local(),
            d(2026, 3, 10).and_hms_opt(0, 0, 0).unwrap()
        );
    }

    #[test]
    fn day_start_uses_the_first_real_instant_when_midnight_is_skipped() {
        // São Paulo sprang forward at midnight on 2017-10-15 (00:00 -> 01:00),
        // so that civil day never had a 00:00. Old code panicked here.
        let tz = chrono_tz::America::Sao_Paulo;
        assert_eq!(
            day_start_in(&tz, d(2017, 10, 15)).naive_local(),
            d(2017, 10, 15).and_hms_opt(1, 0, 0).unwrap()
        );
    }

    #[test]
    fn day_start_survives_a_wholly_skipped_civil_date() {
        // Pacific/Apia skipped all of 2011-12-30 for the date-line change; the
        // requirement is a real instant rather than a panic.
        let tz = chrono_tz::Pacific::Apia;
        let start = day_start_in(&tz, d(2011, 12, 30));
        assert!(start.naive_local() >= d(2011, 12, 30).and_hms_opt(0, 0, 0).unwrap());
    }
}
