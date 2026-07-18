//! Event recurrence rules.
//!
//! Calix models a small set of simple repeat frequencies that round-trip to an
//! iCalendar `RRULE` value — the representation both Google Calendar (a
//! `recurrence` array of `"RRULE:…"` strings) and CalDAV (an `RRULE:` line in
//! the `VEVENT`) speak natively. An event's recurrence is an
//! [`Option<Frequency>`]; `None` is a one-off event. Richer rules (intervals,
//! `BYDAY`, `UNTIL`/`COUNT`) can grow from here.
//!
//! [`occurrences_in`] expands a recurring event into its concrete occurrences
//! within a range, so local events (which no server expands for us) still
//! repeat on the grid.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone};

/// How often an event repeats. Deliberately narrow: these are exactly the rules
/// Calix can author from its repeat picker and recover unambiguously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    /// Every frequency, in the order the repeat picker lists them.
    pub const ALL: [Frequency; 4] = [
        Frequency::Daily,
        Frequency::Weekly,
        Frequency::Monthly,
        Frequency::Yearly,
    ];

    /// Human-readable label for the repeat picker.
    pub fn label(self) -> &'static str {
        match self {
            Frequency::Daily => "Daily",
            Frequency::Weekly => "Weekly",
            Frequency::Monthly => "Monthly",
            Frequency::Yearly => "Yearly",
        }
    }

    /// Maps a repeat-picker row index back to a recurrence. The picker lists
    /// "Does not repeat" at index 0, then [`Frequency::ALL`] in order, so index
    /// 0 (and any out-of-range index) is a one-off event.
    pub fn from_picker_index(index: u32) -> Option<Frequency> {
        index
            .checked_sub(1)
            .and_then(|offset| Frequency::ALL.get(offset as usize).copied())
    }

    /// The repeat-picker row index for a recurrence — the inverse of
    /// [`Frequency::from_picker_index`]. A one-off event is index 0.
    pub fn picker_index(recurrence: Option<Frequency>) -> u32 {
        match recurrence {
            None => 0,
            Some(freq) => Frequency::ALL
                .iter()
                .position(|candidate| *candidate == freq)
                .map_or(0, |offset| offset as u32 + 1),
        }
    }

    /// The iCalendar `RRULE` value for this frequency — the text after
    /// `RRULE:`, e.g. `"FREQ=WEEKLY"`.
    pub fn to_rrule(self) -> String {
        let freq = match self {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        };
        format!("FREQ={freq}")
    }

    /// Recover a [`Frequency`] from a stored `RRULE` value. A leading `RRULE:`
    /// is optional and parsing is case-insensitive. Only an *exact* single
    /// `FREQ=…` rule is recognised — anything carrying extra components (an
    /// `INTERVAL`, a `BYDAY`, a `COUNT`) returns `None`, so a rule Calix cannot
    /// faithfully represent is treated as unknown rather than silently
    /// downgraded to a plain frequency.
    pub fn from_rrule(rule: &str) -> Option<Frequency> {
        let rule = rule.trim().to_ascii_uppercase();
        let rule = rule.strip_prefix("RRULE:").unwrap_or(&rule);
        // An exact single `FREQ=…` rule only: extra components mean a rule we
        // cannot faithfully round-trip, so we decline to recognise it.
        let freq = rule.strip_prefix("FREQ=")?;
        if freq.contains(';') {
            return None;
        }
        match freq {
            "DAILY" => Some(Frequency::Daily),
            "WEEKLY" => Some(Frequency::Weekly),
            "MONTHLY" => Some(Frequency::Monthly),
            "YEARLY" => Some(Frequency::Yearly),
            _ => None,
        }
    }
}

/// The start instants of `freq`'s occurrences whose `[start, start + duration)`
/// span overlaps `[range_start, range_end)`, beginning from `base_start`.
///
/// Occurrences step on the civil calendar and keep `base_start`'s wall-clock
/// time, so a 9am event stays 9am across a DST change. A monthly/yearly rule
/// only lands on months (or years) that actually have `base_start`'s day —
/// matching how RFC 5545 expands `FREQ=MONTHLY`/`YEARLY` without a `BYMONTHDAY`
/// (so the 31st skips short months, and Feb 29 skips common years).
pub fn occurrences_in<Tz: TimeZone>(
    base_start: DateTime<Tz>,
    duration: Duration,
    freq: Frequency,
    range_start: DateTime<Tz>,
    range_end: DateTime<Tz>,
) -> Vec<DateTime<Tz>> {
    let tz = base_start.timezone();
    let base_date = base_start.date_naive();
    let base_time = base_start.time();

    // Jump the index close to the range so an ancient base isn't stepped through
    // one occurrence at a time; start a step early to catch an occurrence that
    // begins just before the range yet spans into it.
    let start_index = approximate_start_index(base_date, freq, range_start.date_naive());
    // Enough headroom for any realistic viewport even after the fast-forward;
    // a hard stop so a degenerate call can never loop unbounded.
    let last_index = start_index + 4000;

    let mut occurrences = Vec::new();
    let mut index = start_index;
    while index <= last_index {
        if let Some(date) = occurrence_date(base_date, freq, index) {
            let start = resolve_forward(&tz, date, base_time);
            if start >= range_end {
                break;
            }
            if start.clone() + duration > range_start {
                occurrences.push(start);
            }
        }
        index += 1;
    }
    occurrences
}

/// The occurrence index at (or just before) `range_start`, so expansion can skip
/// straight there instead of iterating from `base`. Approximate on purpose —
/// callers step forward from here — so month/year skips need no accounting.
fn approximate_start_index(base: NaiveDate, freq: Frequency, range_start: NaiveDate) -> i64 {
    let index = match freq {
        Frequency::Daily => (range_start - base).num_days(),
        Frequency::Weekly => (range_start - base).num_days().div_euclid(7),
        Frequency::Monthly => months_between(base, range_start),
        Frequency::Yearly => (range_start.year() - base.year()) as i64,
    };
    (index - 1).max(0)
}

/// The date of occurrence `index` (0-based), or `None` when the rule skips it —
/// a monthly/yearly rule whose day-of-month doesn't exist that month/year.
fn occurrence_date(base: NaiveDate, freq: Frequency, index: i64) -> Option<NaiveDate> {
    match freq {
        Frequency::Daily => Some(base + Duration::days(index)),
        Frequency::Weekly => Some(base + Duration::weeks(index)),
        Frequency::Monthly => {
            let month = base.year() as i64 * 12 + base.month0() as i64 + index;
            NaiveDate::from_ymd_opt(
                month.div_euclid(12) as i32,
                month.rem_euclid(12) as u32 + 1,
                base.day(),
            )
        }
        Frequency::Yearly => {
            NaiveDate::from_ymd_opt(base.year() + index as i32, base.month(), base.day())
        }
    }
}

fn months_between(from: NaiveDate, to: NaiveDate) -> i64 {
    (to.year() - from.year()) as i64 * 12 + (to.month() as i64 - from.month() as i64)
}

/// Resolves a civil date+time to `tz`, preferring the exact instant but walking
/// forward to the first real one if that wall-clock time was skipped by a DST
/// spring-forward — the same policy as [`crate::date_util::day_start_in`].
fn resolve_forward<Tz: TimeZone>(tz: &Tz, date: NaiveDate, time: NaiveTime) -> DateTime<Tz> {
    let naive = date.and_time(time);
    if let Some(instant) = tz.from_local_datetime(&naive).earliest() {
        return instant;
    }
    (1..=48 * 60)
        .find_map(|minutes| {
            tz.from_local_datetime(&(naive + Duration::minutes(minutes)))
                .earliest()
        })
        .unwrap_or_else(|| tz.from_utc_datetime(&naive))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_frequency_serializes_to_a_freq_daily_rule() {
        assert_eq!(Frequency::Daily.to_rrule(), "FREQ=DAILY");
    }

    #[test]
    fn each_frequency_round_trips_through_its_rrule() {
        for freq in Frequency::ALL {
            assert_eq!(Frequency::from_rrule(&freq.to_rrule()), Some(freq));
        }
    }

    #[test]
    fn an_rrule_prefix_is_optional_when_parsing() {
        assert_eq!(
            Frequency::from_rrule("RRULE:FREQ=WEEKLY"),
            Some(Frequency::Weekly)
        );
    }

    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(
            Frequency::from_rrule("freq=monthly"),
            Some(Frequency::Monthly)
        );
    }

    #[test]
    fn a_rule_with_extra_parts_is_not_a_simple_frequency() {
        assert_eq!(Frequency::from_rrule("FREQ=WEEKLY;INTERVAL=2"), None);
    }

    #[test]
    fn an_unknown_frequency_is_not_recognized() {
        assert_eq!(Frequency::from_rrule("FREQ=HOURLY"), None);
    }

    #[test]
    fn empty_or_garbage_rules_are_not_recognized() {
        assert_eq!(Frequency::from_rrule(""), None);
        assert_eq!(Frequency::from_rrule("nonsense"), None);
    }

    #[test]
    fn picker_index_round_trips_through_frequency() {
        assert_eq!(Frequency::from_picker_index(0), None);
        assert_eq!(Frequency::picker_index(None), 0);
        for (offset, freq) in Frequency::ALL.iter().enumerate() {
            let index = offset as u32 + 1;
            assert_eq!(Frequency::from_picker_index(index), Some(*freq));
            assert_eq!(Frequency::picker_index(Some(*freq)), index);
        }
    }

    #[test]
    fn an_out_of_range_picker_index_is_a_one_off() {
        assert_eq!(Frequency::from_picker_index(99), None);
    }

    // A fixed zone keeps expansion tests deterministic regardless of the test
    // machine's timezone, and lets the DST case below assert real offsets.
    const NY: chrono_tz::Tz = chrono_tz::America::New_York;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<chrono_tz::Tz> {
        NY.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    fn occurrence_days(occ: &[DateTime<chrono_tz::Tz>]) -> Vec<NaiveDate> {
        occ.iter().map(|o| o.date_naive()).collect()
    }

    #[test]
    fn daily_expands_each_day_that_overlaps_the_range() {
        // Base is days before the range, so this also exercises the fast-forward.
        let occ = occurrences_in(
            at(2026, 7, 1, 9),
            Duration::hours(1),
            Frequency::Daily,
            at(2026, 7, 5, 0),
            at(2026, 7, 8, 0),
        );
        assert_eq!(
            occurrence_days(&occ),
            [(2026, 7, 5), (2026, 7, 6), (2026, 7, 7)]
                .map(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d).unwrap())
        );
    }

    #[test]
    fn weekly_expands_to_one_occurrence_per_week_in_range() {
        // 2026-07-02 is a Thursday.
        let occ = occurrences_in(
            at(2026, 7, 2, 9),
            Duration::hours(1),
            Frequency::Weekly,
            at(2026, 7, 1, 0),
            at(2026, 8, 1, 0),
        );
        assert_eq!(
            occurrence_days(&occ),
            [2, 9, 16, 23, 30].map(|d| NaiveDate::from_ymd_opt(2026, 7, d).unwrap())
        );
        assert!(
            occ.iter()
                .all(|o| o.time() == NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        );
    }

    #[test]
    fn monthly_on_the_31st_skips_months_without_a_31st() {
        let occ = occurrences_in(
            at(2026, 1, 31, 12),
            Duration::hours(1),
            Frequency::Monthly,
            at(2026, 1, 1, 0),
            at(2026, 7, 1, 0),
        );
        // Feb, Apr and Jun have no 31st, so they are skipped entirely.
        assert_eq!(
            occurrence_days(&occ),
            [1, 3, 5].map(|m| NaiveDate::from_ymd_opt(2026, m, 31).unwrap())
        );
    }

    #[test]
    fn yearly_on_feb_29_only_lands_on_leap_years() {
        let occ = occurrences_in(
            at(2024, 2, 29, 8),
            Duration::hours(1),
            Frequency::Yearly,
            at(2024, 1, 1, 0),
            at(2033, 1, 1, 0),
        );
        let years: Vec<i32> = occ.iter().map(|o| o.date_naive().year()).collect();
        assert_eq!(years, vec![2024, 2028, 2032]);
    }

    #[test]
    fn an_occurrence_that_starts_before_the_range_but_spans_into_it_is_included() {
        // Thu 2026-07-02 23:00 for 2h ends Fri 01:00, overlapping a Fri window.
        let occ = occurrences_in(
            NY.with_ymd_and_hms(2026, 7, 2, 23, 0, 0).unwrap(),
            Duration::hours(2),
            Frequency::Weekly,
            at(2026, 7, 3, 0),
            at(2026, 7, 3, 12),
        );
        assert_eq!(
            occurrence_days(&occ),
            [NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()]
        );
    }

    #[test]
    fn weekly_occurrences_keep_their_wall_clock_time_across_a_dst_change() {
        // US spring-forward is 2026-03-08; a 9am Wednesday event stays 9am both
        // before and after, so the UTC gap between them is 167h, not 168h.
        let occ = occurrences_in(
            at(2026, 3, 4, 9),
            Duration::hours(1),
            Frequency::Weekly,
            at(2026, 3, 1, 0),
            at(2026, 3, 20, 0),
        );
        assert_eq!(occ.len(), 3);
        assert!(
            occ.iter()
                .all(|o| o.time() == NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        );
        assert_eq!((occ[1] - occ[0]).num_hours(), 167);
    }
}
