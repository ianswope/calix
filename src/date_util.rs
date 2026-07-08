use chrono::{Datelike, Months, NaiveDate, Weekday};

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
}
