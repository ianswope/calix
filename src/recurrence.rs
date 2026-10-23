//! Event recurrence rules.
//!
//! Calix stores a recurrence as an iCalendar `RRULE` value — the text Google
//! Calendar puts in a `recurrence` array and CalDAV puts on an `RRULE` line.
//! [`Frequency`] is the simple daily, weekly, monthly, or yearly choice.
//! A rule that also carries an interval, weekdays, a week of the month, or an
//! end is a [`CustomRule`] on [`Frequency::Custom`]. A part Calix cannot
//! expand (`BYSETPOS`, `BYMONTH`, a second end, …) is `None`, so it is never
//! silently downgraded to a plain frequency.
//!
//! [`occurrences_in`] expands a rule into the occurrences inside a range.
//! Local events repeat that way. Synced calendars arrive already expanded by
//! their server.
//!
//! [`occurrences_in`] expands a recurring event into its concrete occurrences
//! within a range, so local events (which no server expands for us) still
//! repeat on the grid.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone};

/// How often an event repeats. These are the rules Calix can author and expand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
    Custom(CustomRule),
}

/// The unit of a [`CustomRule`]. Listed in the custom repeat picker's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomFreq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl CustomFreq {
    /// Row index in the custom frequency picker.
    pub fn index(self) -> u32 {
        match self {
            CustomFreq::Daily => 0,
            CustomFreq::Weekly => 1,
            CustomFreq::Monthly => 2,
            CustomFreq::Yearly => 3,
        }
    }

    #[cfg(feature = "gui")]
    pub(crate) fn from_index(index: u32) -> Option<Self> {
        Some(match index {
            0 => CustomFreq::Daily,
            1 => CustomFreq::Weekly,
            2 => CustomFreq::Monthly,
            3 => CustomFreq::Yearly,
            _ => return None,
        })
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "DAILY" => CustomFreq::Daily,
            "WEEKLY" => CustomFreq::Weekly,
            "MONTHLY" => CustomFreq::Monthly,
            "YEARLY" => CustomFreq::Yearly,
            _ => return None,
        })
    }

    fn rrule_name(self) -> &'static str {
        match self {
            CustomFreq::Daily => "DAILY",
            CustomFreq::Weekly => "WEEKLY",
            CustomFreq::Monthly => "MONTHLY",
            CustomFreq::Yearly => "YEARLY",
        }
    }

    fn unit(self) -> (&'static str, &'static str) {
        match self {
            CustomFreq::Daily => ("day", "days"),
            CustomFreq::Weekly => ("week", "weeks"),
            CustomFreq::Monthly => ("month", "months"),
            CustomFreq::Yearly => ("year", "years"),
        }
    }
}

/// Validated custom recurrence. Weekdays are Monday-based bits. A non-zero
/// ordinal applies to a monthly `BYDAY` (for example `3TU` or `-1FR`); zero
/// means every selected weekday in the month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRule {
    pub frequency: CustomFreq,
    pub interval: u16,
    pub weekdays: u8,
    pub ordinal: i8,
    pub count: Option<u32>,
    pub until: Option<NaiveDate>,
}

/// What the custom repeat controls are asking for, before it is validated.
///
/// Weekdays are Monday-based bits. An empty set means the event's own weekday
/// (weekly) or its own day of the month (monthly). `ordinal` is the week of
/// the month: `1` through `4`, `-1` for the last week, or `0` for every
/// selected weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRepeatSpec {
    pub frequency: CustomFreq,
    pub interval: u16,
    pub weekdays: u8,
    pub ordinal: i8,
    pub count: Option<u32>,
    pub until: Option<NaiveDate>,
}

const DAY_NAMES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl CustomRepeatSpec {
    /// The frequency this spec stores, or a message naming what to fix.
    pub fn build(self) -> Result<Frequency, &'static str> {
        self.validated().map(CustomRule::into_frequency)
    }

    /// A sentence for the custom row's subtitle. An incomplete spec produces
    /// the same message [`Self::build`] would return.
    pub fn summary(self) -> String {
        match self.validated() {
            Ok(rule) => rule.summary(),
            Err(message) => message.to_string(),
        }
    }

    fn validated(self) -> Result<CustomRule, &'static str> {
        if self.interval == 0 {
            return Err("Repeat interval has to be at least 1");
        }
        if self.count.is_some() && self.until.is_some() {
            return Err("Choose an end date or a number of events, not both");
        }
        if self.count.is_some_and(|count| count == 0) {
            return Err("The number of events has to be at least 1");
        }
        if self.ordinal != 0 && self.frequency != CustomFreq::Monthly {
            return Err("A week of the month only applies to a monthly repeat");
        }
        if !(-1..=4).contains(&self.ordinal) {
            return Err("That week of the month isn't supported");
        }
        if self.ordinal != 0 && self.weekdays == 0 {
            return Err("Choose at least one weekday");
        }
        let weekdays = match self.frequency {
            CustomFreq::Daily | CustomFreq::Yearly => 0,
            CustomFreq::Weekly | CustomFreq::Monthly => self.weekdays,
        };
        Ok(CustomRule {
            frequency: self.frequency,
            interval: self.interval,
            weekdays,
            ordinal: if self.frequency == CustomFreq::Monthly {
                self.ordinal
            } else {
                0
            },
            count: self.count,
            until: self.until,
        })
    }
}

impl CustomRule {
    fn parse(text: &str) -> Option<Self> {
        let mut rule = Self {
            frequency: CustomFreq::Daily,
            interval: 1,
            weekdays: 0,
            ordinal: 0,
            count: None,
            until: None,
        };
        let mut seen_freq = false;
        let mut seen = std::collections::HashSet::new();
        for part in text.split(';') {
            let (key, value) = part.split_once('=')?;
            if !seen.insert(key) {
                return None;
            }
            match key {
                "FREQ" => {
                    rule.frequency = CustomFreq::from_name(value)?;
                    seen_freq = true;
                }
                "INTERVAL" => {
                    rule.interval = value.parse().ok()?;
                    if rule.interval == 0 {
                        return None;
                    }
                }
                "COUNT" => {
                    let count = value.parse::<u32>().ok()?;
                    if count == 0 {
                        return None;
                    }
                    rule.count = Some(count);
                }
                "UNTIL" => rule.until = Some(parse_until_date(value)?),
                "BYDAY" => {
                    if value.is_empty() {
                        return None;
                    }
                    for day in value.split(',') {
                        if day.len() < 2 || !day.is_ascii() {
                            return None;
                        }
                        let (ordinal_text, name) = day.split_at(day.len() - 2);
                        let index = DAY_NAMES.iter().position(|candidate| *candidate == name)?;
                        let ordinal = if ordinal_text.is_empty() {
                            0
                        } else {
                            let ordinal = ordinal_text.parse::<i8>().ok()?;
                            // A written zero (`0TU`) is not "no ordinal"; RFC
                            // 5545 ordinals start at 1 and -1.
                            if ordinal == 0 || !(-5..=5).contains(&ordinal) {
                                return None;
                            }
                            ordinal
                        };
                        if rule.weekdays != 0 && rule.ordinal != ordinal {
                            return None;
                        }
                        rule.ordinal = ordinal;
                        rule.weekdays |= 1 << index;
                    }
                }
                _ => return None,
            }
        }
        if !seen_freq
            || (rule.count.is_some() && rule.until.is_some())
            || (rule.ordinal != 0 && rule.frequency != CustomFreq::Monthly)
            || (rule.frequency == CustomFreq::Yearly && rule.weekdays != 0)
        {
            return None;
        }
        Some(rule)
    }

    fn into_frequency(self) -> Frequency {
        if self.interval == 1
            && self.weekdays == 0
            && self.ordinal == 0
            && self.count.is_none()
            && self.until.is_none()
        {
            return match self.frequency {
                CustomFreq::Daily => Frequency::Daily,
                CustomFreq::Weekly => Frequency::Weekly,
                CustomFreq::Monthly => Frequency::Monthly,
                CustomFreq::Yearly => Frequency::Yearly,
            };
        }
        Frequency::Custom(self)
    }

    fn to_rrule(self) -> String {
        self.write_rrule(self.until.map(|day| day.format("%Y%m%d").to_string()))
    }

    fn write_rrule(self, until: Option<String>) -> String {
        let mut text = format!("FREQ={}", self.frequency.rrule_name());
        if self.interval != 1 {
            text.push_str(&format!(";INTERVAL={}", self.interval));
        }
        if self.weekdays != 0 {
            let days = DAY_NAMES
                .iter()
                .enumerate()
                .filter(|(index, _)| self.weekdays & (1 << index) != 0)
                .map(|(_, name)| {
                    if self.ordinal == 0 {
                        (*name).to_string()
                    } else {
                        format!("{}{name}", self.ordinal)
                    }
                })
                .collect::<Vec<_>>();
            text.push_str(&format!(";BYDAY={}", days.join(",")));
        }
        if let Some(count) = self.count {
            text.push_str(&format!(";COUNT={count}"));
        }
        if let Some(until) = until {
            text.push_str(&format!(";UNTIL={until}"));
        }
        text
    }

    fn summary(self) -> String {
        let (singular, plural) = self.frequency.unit();
        let mut text = if self.interval <= 1 {
            format!("Every {singular}")
        } else {
            format!("Every {} {plural}", self.interval)
        };
        match self.frequency {
            CustomFreq::Daily => {}
            CustomFreq::Weekly if self.weekdays == 0 => {
                text.push_str(" on the event's weekday");
            }
            CustomFreq::Weekly => {
                text.push_str(" on ");
                text.push_str(&weekday_list(self.weekdays));
            }
            CustomFreq::Monthly | CustomFreq::Yearly if self.weekdays == 0 => {
                text.push_str(" on this date");
            }
            CustomFreq::Monthly => {
                text.push(' ');
                text.push_str(&month_weekday_phrase(self.ordinal, self.weekdays));
            }
            // A yearly rule never carries weekdays; the arm keeps the match
            // exhaustive for a hand-built rule.
            CustomFreq::Yearly => {}
        }
        if let Some(count) = self.count {
            match count {
                1 => text.push_str(", once"),
                2 => text.push_str(", twice"),
                _ => text.push_str(&format!(", {count} times")),
            }
        }
        if let Some(until) = self.until {
            text.push_str(&format!(", until {}", english_date(until)));
        }
        text
    }

    fn matches(self, base: NaiveDate, date: NaiveDate) -> bool {
        if self.interval == 0 || date < base {
            return false;
        }
        let interval = i64::from(self.interval);
        let on_day = self.weekdays & (1 << date.weekday().num_days_from_monday()) != 0;
        match self.frequency {
            CustomFreq::Daily => {
                (date - base).num_days() % interval == 0 && (self.weekdays == 0 || on_day)
            }
            CustomFreq::Weekly => {
                let monday = |day: NaiveDate| {
                    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
                };
                (monday(date) - monday(base)).num_weeks() % interval == 0
                    && if self.weekdays == 0 {
                        date.weekday() == base.weekday()
                    } else {
                        on_day
                    }
            }
            CustomFreq::Monthly => {
                if months_between(base, date) % interval != 0 {
                    return false;
                }
                if self.weekdays == 0 {
                    return date.day() == base.day();
                }
                if !on_day {
                    return false;
                }
                match self.ordinal {
                    0 => true,
                    n if n > 0 => (date.day() - 1) / 7 + 1 == n as u32,
                    n => {
                        week_from_month_end(date).is_some_and(|from_end| from_end == -i64::from(n))
                    }
                }
            }
            CustomFreq::Yearly => {
                let years = i64::from(date.year() - base.year());
                years % interval == 0 && date.month() == base.month() && date.day() == base.day()
            }
        }
    }
}

fn parse_until_date(value: &str) -> Option<NaiveDate> {
    let date = value.get(..8).filter(|text| text.len() == 8)?;
    if value.len() != 8 {
        return None;
    }
    NaiveDate::parse_from_str(date, "%Y%m%d").ok()
}

fn week_from_month_end(date: NaiveDate) -> Option<i64> {
    let next_month = date
        .with_day(1)?
        .checked_add_months(chrono::Months::new(1))?;
    Some(((next_month - date).num_days() - 1) / 7 + 1)
}

fn weekday_list(bits: u8) -> String {
    let days: Vec<&str> = WEEKDAY_NAMES
        .iter()
        .enumerate()
        .filter(|(index, _)| bits & (1 << index) != 0)
        .map(|(_, name)| *name)
        .collect();
    match days.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [first, second] => format!("{first} and {second}"),
        many => {
            let (last, rest) = many.split_last().expect("at least three weekdays");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn month_weekday_phrase(ordinal: i8, weekdays: u8) -> String {
    let days = weekday_list(weekdays);
    if ordinal == 0 {
        return format!("on each {days}");
    }
    let which = match ordinal {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        _ => "last",
    };
    format!("on the {which} {days}")
}

fn english_date(date: NaiveDate) -> String {
    format!(
        "{} {}, {}",
        MONTH_NAMES[date.month0() as usize],
        date.day(),
        date.year()
    )
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
            Frequency::Custom(_) => "Custom…",
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

    /// "Custom…" follows "Does not repeat" and [`Frequency::ALL`].
    pub const CUSTOM_PICKER_INDEX: u32 = (Frequency::ALL.len() as u32) + 1;

    /// The repeat-picker row index for a recurrence — the inverse of
    /// [`Frequency::from_picker_index`] for a simple frequency. A one-off
    /// event is index 0, and every custom rule shares [`Self::CUSTOM_PICKER_INDEX`].
    pub fn picker_index(recurrence: Option<Frequency>) -> u32 {
        match recurrence {
            None => 0,
            Some(Frequency::Custom(_)) => Frequency::CUSTOM_PICKER_INDEX,
            Some(freq) => Frequency::ALL
                .iter()
                .position(|candidate| *candidate == freq)
                .map_or(0, |offset| offset as u32 + 1),
        }
    }

    /// The iCalendar `RRULE` value stored locally and expanded by
    /// [`occurrences_in`]. The text after `RRULE:`, for example `"FREQ=WEEKLY"`.
    /// An end date is a calendar date (`UNTIL=20260731`), inclusive.
    pub fn to_rrule(self) -> String {
        let freq = match self {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
            Frequency::Custom(rule) => return rule.to_rrule(),
        };
        format!("FREQ={freq}")
    }

    /// The `RRULE` value to send a provider. A timed event's `UNTIL` has to be
    /// a UTC date-time when `DTSTART` is a date-time (RFC 5545). The instant
    /// is the end of that local calendar day, so an evening occurrence on the
    /// stop date is still included. All-day events, and rules with no end
    /// date, keep [`Self::to_rrule`].
    pub fn to_provider_rrule<Tz: TimeZone>(self, all_day: bool, start: &DateTime<Tz>) -> String {
        let Frequency::Custom(rule) = self else {
            return self.to_rrule();
        };
        let Some(until) = rule.until.filter(|_| !all_day) else {
            return self.to_rrule();
        };
        let end_of_day = crate::date_util::resolve_forward(
            &start.timezone(),
            until.and_time(NaiveTime::from_hms_opt(23, 59, 59).expect("23:59:59")),
        );
        let utc = end_of_day
            .with_timezone(&chrono::Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        rule.write_rrule(Some(utc))
    }

    /// Recover a [`Frequency`] from a stored `RRULE` value. A leading `RRULE:`
    /// is optional and parsing is case-insensitive. An interval of 1 with no
    /// other part is the simple frequency. Supported custom parts (`INTERVAL`,
    /// `BYDAY`, `COUNT`, `UNTIL`) become [`Frequency::Custom`]. Anything else
    /// is `None`.
    pub fn from_rrule(rule: &str) -> Option<Frequency> {
        let rule = rule.trim().to_ascii_uppercase();
        let rule = rule.strip_prefix("RRULE:").unwrap_or(&rule);
        // Parts may arrive in any order. A bare `FREQ=` is the simple form;
        // anything else has to parse as a custom rule or be declined.
        if !rule.contains(';') {
            let freq = rule.strip_prefix("FREQ=")?;
            return CustomFreq::from_name(freq).map(|frequency| {
                CustomRule {
                    frequency,
                    interval: 1,
                    weekdays: 0,
                    ordinal: 0,
                    count: None,
                    until: None,
                }
                .into_frequency()
            });
        }
        CustomRule::parse(rule).map(CustomRule::into_frequency)
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
    occurrences_with_end_in(
        base_start,
        duration,
        freq,
        range_start,
        range_end,
        |start| start.clone() + duration,
    )
}

/// Expand with the same end calculation used to render each occurrence.
/// `duration` controls the lookback; `end_at` decides actual overlap, allowing
/// all-day events to end at local midnight even across a DST transition.
pub(crate) fn occurrences_with_end_in<Tz: TimeZone>(
    base_start: DateTime<Tz>,
    duration: Duration,
    freq: Frequency,
    range_start: DateTime<Tz>,
    range_end: DateTime<Tz>,
    end_at: impl Fn(&DateTime<Tz>) -> DateTime<Tz>,
) -> Vec<DateTime<Tz>> {
    if let Frequency::Custom(rule) = freq {
        let base = base_start.date_naive();
        // A count is measured from the first occurrence, so the scan has to
        // start at the anchor even when that is far before the window. Any
        // other rule can start at the earliest day that might still be running
        // when the window opens.
        let first = if rule.count.is_some() {
            base
        } else {
            range_start
                .clone()
                .checked_sub_signed(duration)
                .and_then(|instant| instant.checked_sub_signed(Duration::days(1)))
                .map(|instant| instant.date_naive())
                .unwrap_or(base)
                .max(base)
        };
        let mut date = first;
        let mut count = 0u32;
        let mut scanned = 0i64;
        let mut result = Vec::new();
        while date <= range_end.date_naive() {
            // A count that starts decades ago still walks day by day. Stop
            // rather than hang if the window itself is absurd.
            if scanned >= MAX_CUSTOM_DAYS {
                break;
            }
            scanned += 1;
            if rule.until.is_some_and(|until| date > until) {
                break;
            }
            if rule.matches(base, date) {
                count += 1;
                if rule.count.is_some_and(|limit| count > limit) {
                    break;
                }
                let start = resolve_forward(&base_start.timezone(), date, base_start.time());
                if start >= range_end {
                    break;
                }
                if end_at(&start) > range_start {
                    result.push(start);
                }
            }
            let Some(next) = date.succ_opt() else {
                break;
            };
            date = next;
        }
        return result;
    }
    let tz = base_start.timezone();
    let base_date = base_start.date_naive();
    let base_time = base_start.time();

    // Jump the index close to the range so an ancient base isn't stepped through
    // one occurrence at a time.
    let range_index = approximate_index_at(base_date, freq, range_start.date_naive());
    // Then back up far enough to catch the occurrences that began before the
    // range and are still running when it opens. One step covers the ordinary
    // event, shorter than its own repeat interval; an event longer than that has
    // several in flight at once.
    let start_index = (range_index - steps_spanning(duration, freq)).max(0);
    // Enough headroom for any realistic viewport even after the fast-forward; a
    // hard stop so a degenerate call can never loop unbounded. Anchored to the
    // range rather than to `start_index`, so backing up can't eat into it.
    let last_index = range_index + MAX_STEPS;

    let mut occurrences = Vec::new();
    let mut index = start_index;
    while index <= last_index {
        if let Some(date) = occurrence_date(base_date, freq, index) {
            let start = resolve_forward(&tz, date, base_time);
            if start >= range_end {
                break;
            }
            if end_at(&start) > range_start {
                occurrences.push(start);
            }
        }
        index += 1;
    }
    occurrences
}

/// Hard stop on how far one call will step: enough headroom for any realistic
/// viewport, and for an event spanning many of its own intervals, while keeping
/// a degenerate call from looping unbounded.
const MAX_STEPS: i64 = 4000;

/// Day-by-day custom expansion stops here. Long enough for a count that began
/// a lifetime ago and is viewed today, short of walking the whole civil calendar.
const MAX_CUSTOM_DAYS: i64 = 50_000;

/// The occurrence index nearest `range_start`, so expansion can skip straight
/// there instead of iterating from `base`. Approximate on purpose — callers step
/// out from here in both directions — so month/year skips need no accounting.
fn approximate_index_at(base: NaiveDate, freq: Frequency, range_start: NaiveDate) -> i64 {
    match freq {
        Frequency::Daily => (range_start - base).num_days(),
        Frequency::Weekly => (range_start - base).num_days().div_euclid(7),
        Frequency::Monthly => months_between(base, range_start),
        Frequency::Yearly => (range_start.year() - base.year()) as i64,
        Frequency::Custom(_) => unreachable!("custom rules expand separately"),
    }
}

/// How many recurrence steps back cover an event of `duration`, so expansion
/// starts at the earliest occurrence that could still be running when the range
/// opens.
///
/// Measured against the *shortest* interval the frequency can have — a 28-day
/// month, a 365-day year — so the count is only ever generous: an occurrence
/// that turns out not to overlap is filtered out a line later, while one missed
/// is an event silently absent from the grid.
fn steps_spanning(duration: Duration, freq: Frequency) -> i64 {
    let interval_days = match freq {
        Frequency::Daily => 1,
        Frequency::Weekly => 7,
        Frequency::Monthly => 28,
        Frequency::Yearly => 365,
        Frequency::Custom(_) => unreachable!("custom rules expand separately"),
    };
    (duration.num_days().max(0) / interval_days + 1).min(MAX_STEPS)
}

/// The date of occurrence `index` (0-based), or `None` when the rule skips it —
/// a monthly/yearly rule whose day-of-month doesn't exist that month/year.
fn occurrence_date(base: NaiveDate, freq: Frequency, index: i64) -> Option<NaiveDate> {
    match freq {
        Frequency::Custom(_) => None,
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

/// Resolves a civil date+time to `tz`, walking forward past a DST gap — the
/// one policy for skipped and repeated wall-clock times, shared with
/// [`crate::date_util::day_start_in`].
fn resolve_forward<Tz: TimeZone>(tz: &Tz, date: NaiveDate, time: NaiveTime) -> DateTime<Tz> {
    crate::date_util::resolve_forward(tz, date.and_time(time))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn spec(
        frequency: CustomFreq,
        interval: u16,
        weekdays: u8,
        ordinal: i8,
        count: Option<u32>,
        until: Option<NaiveDate>,
    ) -> CustomRepeatSpec {
        CustomRepeatSpec {
            frequency,
            interval,
            weekdays,
            ordinal,
            count,
            until,
        }
    }

    #[test]
    fn an_interval_of_one_with_nothing_else_is_the_simple_frequency() {
        assert_eq!(
            Frequency::from_rrule("FREQ=WEEKLY;INTERVAL=1"),
            Some(Frequency::Weekly)
        );
        assert_eq!(
            Frequency::from_rrule(&Frequency::Weekly.to_rrule()),
            Some(Frequency::Weekly)
        );
        assert_eq!(
            spec(CustomFreq::Weekly, 1, 0, 0, None, None).build(),
            Ok(Frequency::Weekly)
        );
        // A numeric zero is not "no ordinal".
        assert!(Frequency::from_rrule("FREQ=MONTHLY;BYDAY=0TU").is_none());
    }

    #[test]
    fn a_real_custom_rule_round_trips_including_an_interval_of_one() {
        for text in [
            "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE",
            "INTERVAL=2;FREQ=WEEKLY",
            "freq=weekly;byday=mo,we",
            "RRULE:FREQ=MONTHLY;BYDAY=-1FR",
        ] {
            let rule = Frequency::from_rrule(text).expect(text);
            assert_eq!(
                Frequency::from_rrule(&rule.to_rrule()),
                Some(rule),
                "{text}"
            );
            assert!(matches!(rule, Frequency::Custom(_)), "{text}");
        }
    }

    #[test]
    fn custom_parts_say_what_they_repeat_and_reject_an_incomplete_month() {
        assert_eq!(
            spec(CustomFreq::Weekly, 2, 0, 0, None, None).summary(),
            "Every 2 weeks on the event's weekday"
        );
        assert_eq!(
            spec(
                CustomFreq::Weekly,
                1,
                1 | (1 << 2) | (1 << 4),
                0,
                None,
                None
            )
            .summary(),
            "Every week on Monday, Wednesday, and Friday"
        );
        assert_eq!(
            spec(CustomFreq::Monthly, 2, 1 << 1, 3, None, None).summary(),
            "Every 2 months on the third Tuesday"
        );
        assert_eq!(
            spec(CustomFreq::Monthly, 1, 1 | (1 << 4), 0, None, None).summary(),
            "Every month on each Monday and Friday"
        );
        assert_eq!(
            spec(CustomFreq::Monthly, 1, 0, 0, None, None).summary(),
            "Every month on this date"
        );
        assert_eq!(
            spec(CustomFreq::Yearly, 4, 0, 0, None, None).summary(),
            "Every 4 years on this date"
        );
        assert_eq!(
            spec(CustomFreq::Daily, 3, 0, 0, Some(10), None).summary(),
            "Every 3 days, 10 times"
        );
        assert_eq!(
            spec(CustomFreq::Daily, 1, 0, 0, Some(1), None).summary(),
            "Every day, once"
        );
        assert_eq!(
            spec(CustomFreq::Daily, 1, 0, 0, Some(2), None).summary(),
            "Every day, twice"
        );
        assert_eq!(
            spec(CustomFreq::Weekly, 2, 0, 0, None, Some(d(2026, 7, 31))).summary(),
            "Every 2 weeks on the event's weekday, until July 31, 2026"
        );
        assert_eq!(
            spec(CustomFreq::Monthly, 1, 0, 3, None, None).build(),
            Err("Choose at least one weekday")
        );
        assert_eq!(
            spec(CustomFreq::Weekly, 2, 0, 0, Some(3), Some(d(2026, 7, 31))).summary(),
            "Choose an end date or a number of events, not both"
        );
        assert_eq!(
            spec(CustomFreq::Daily, 0, 0, 0, None, None).build(),
            Err("Repeat interval has to be at least 1")
        );
    }

    #[test]
    fn a_custom_rule_uses_the_custom_picker_row() {
        let rule = Frequency::from_rrule("FREQ=WEEKLY;INTERVAL=2").unwrap();
        assert_eq!(Frequency::CUSTOM_PICKER_INDEX, 5);
        assert_eq!(
            Frequency::picker_index(Some(rule)),
            Frequency::CUSTOM_PICKER_INDEX
        );
        // The simple-frequency mapping has no custom row; the dialog reads
        // the custom controls itself when this index is selected.
        assert_eq!(
            Frequency::from_picker_index(Frequency::CUSTOM_PICKER_INDEX),
            None
        );
    }

    #[test]
    fn a_timed_until_covers_the_stop_date_in_the_events_zone() {
        let rule = Frequency::from_rrule("FREQ=MONTHLY;INTERVAL=2;UNTIL=20260731").unwrap();
        // July 31, 2026 is EDT (UTC-4). 23:59:59 local is 03:59:59 the next
        // day in UTC, which still includes an evening occurrence on the 31st.
        let start = NY.with_ymd_and_hms(2026, 1, 31, 9, 0, 0).unwrap();
        assert_eq!(
            rule.to_provider_rrule(false, &start),
            "FREQ=MONTHLY;INTERVAL=2;UNTIL=20260801T035959Z"
        );
        assert_eq!(
            rule.to_provider_rrule(true, &start),
            "FREQ=MONTHLY;INTERVAL=2;UNTIL=20260731"
        );
        assert_eq!(rule.to_rrule(), "FREQ=MONTHLY;INTERVAL=2;UNTIL=20260731");
        assert_eq!(
            Frequency::Weekly.to_provider_rrule(false, &start),
            "FREQ=WEEKLY"
        );
    }

    #[test]
    fn custom_rules_cover_fortnightly_weekdays_and_the_third_tuesday() {
        for (text, expected) in [
            (
                "FREQ=WEEKLY;INTERVAL=2",
                vec![d(2026, 7, 7), d(2026, 7, 21)],
            ),
            (
                "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;COUNT=4",
                vec![d(2026, 7, 7), d(2026, 7, 8), d(2026, 7, 9), d(2026, 7, 10)],
            ),
            ("FREQ=MONTHLY;BYDAY=3TU", vec![d(2026, 7, 21)]),
        ] {
            let rule = Frequency::from_rrule(text).expect("a supported custom rule");
            assert_eq!(Frequency::from_rrule(&rule.to_rrule()), Some(rule));
            assert_eq!(
                occurrence_days(&occurrences_in(
                    at(2026, 7, 7, 9),
                    Duration::hours(1),
                    rule,
                    at(2026, 7, 1, 0),
                    at(2026, 8, 1, 0)
                )),
                expected
            );
        }
    }

    #[test]
    fn custom_rules_stop_at_their_until_date_and_skip_missing_month_days() {
        let rule =
            Frequency::from_rrule("FREQ=MONTHLY;INTERVAL=2;UNTIL=20260731").expect("custom rule");
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 1, 31, 9),
                Duration::hours(1),
                rule,
                at(2026, 1, 1, 0),
                at(2027, 1, 1, 0)
            )),
            vec![
                d(2026, 1, 31),
                d(2026, 3, 31),
                d(2026, 5, 31),
                d(2026, 7, 31)
            ]
        );
        for invalid in [
            "FREQ=WEEKLY;INTERVAL=0",
            "FREQ=DAILY;COUNT=0",
            "FREQ=MONTHLY;BYDAY=6TU",
            "FREQ=DAILY;COUNT=3;UNTIL=20260731",
            "FREQ=WEEKLY;BYDAY=XX",
            "FREQ=WEEKLY;BYDAY=1MO",
            "FREQ=YEARLY;BYDAY=MO",
            "FREQ=WEEKLY;INTERVAL=2;",
        ] {
            assert_eq!(Frequency::from_rrule(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn a_count_is_spent_from_the_first_occurrence_even_before_the_window() {
        let rule = Frequency::from_rrule("FREQ=DAILY;COUNT=5").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 7, 1, 9),
                Duration::hours(1),
                rule,
                at(2026, 7, 4, 0),
                at(2026, 7, 8, 0)
            )),
            vec![d(2026, 7, 4), d(2026, 7, 5)]
        );
        let spent = Frequency::from_rrule("FREQ=DAILY;COUNT=3").unwrap();
        assert!(
            occurrences_in(
                at(2026, 7, 1, 9),
                Duration::hours(1),
                spent,
                at(2026, 7, 10, 0),
                at(2026, 7, 12, 0)
            )
            .is_empty()
        );
    }

    #[test]
    fn fortnightly_weekdays_skip_the_off_week_and_the_days_before_the_start() {
        // 2026-07-08 is a Wednesday. Monday the 6th is the same week but before
        // the anchor, so it is not an occurrence.
        let rule = Frequency::from_rrule("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 7, 8, 9),
                Duration::hours(1),
                rule,
                at(2026, 7, 1, 0),
                at(2026, 8, 1, 0)
            )),
            vec![d(2026, 7, 8), d(2026, 7, 20), d(2026, 7, 22)]
        );
    }

    #[test]
    fn monthly_weekdays_can_be_every_one_in_the_month_or_the_last() {
        let every_tuesday = Frequency::from_rrule("FREQ=MONTHLY;BYDAY=TU").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 7, 7, 9),
                Duration::hours(1),
                every_tuesday,
                at(2026, 7, 1, 0),
                at(2026, 8, 1, 0)
            )),
            vec![
                d(2026, 7, 7),
                d(2026, 7, 14),
                d(2026, 7, 21),
                d(2026, 7, 28)
            ]
        );
        // February 2026 Fridays are the 6th, 13th, 20th and 27th.
        let second_last = Frequency::from_rrule("FREQ=MONTHLY;BYDAY=-2FR").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 2, 1, 9),
                Duration::hours(1),
                second_last,
                at(2026, 2, 1, 0),
                at(2026, 3, 1, 0)
            )),
            vec![d(2026, 2, 20)]
        );
        let last_friday = Frequency::from_rrule("FREQ=MONTHLY;BYDAY=-1FR").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 2, 1, 9),
                Duration::hours(1),
                last_friday,
                at(2026, 2, 1, 0),
                at(2026, 3, 1, 0)
            )),
            vec![d(2026, 2, 27)]
        );
    }

    #[test]
    fn custom_rules_keep_wall_clock_time_and_include_an_overlap_from_the_day_before() {
        let wednesdays = Frequency::from_rrule("FREQ=WEEKLY;BYDAY=WE").unwrap();
        let occ = occurrences_in(
            at(2026, 3, 4, 9),
            Duration::hours(1),
            wednesdays,
            at(2026, 3, 1, 0),
            at(2026, 3, 20, 0),
        );
        assert_eq!(occ.len(), 3);
        assert!(
            occ.iter()
                .all(|occurrence| occurrence.time() == NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        );
        assert_eq!((occ[1] - occ[0]).num_hours(), 167);

        let overnight = occurrences_in(
            NY.with_ymd_and_hms(2026, 7, 2, 23, 0, 0).unwrap(),
            Duration::hours(2),
            Frequency::from_rrule("FREQ=WEEKLY;INTERVAL=2").unwrap(),
            at(2026, 7, 3, 0),
            at(2026, 7, 3, 12),
        );
        assert_eq!(occurrence_days(&overnight), vec![d(2026, 7, 2)]);
    }

    #[test]
    fn a_custom_yearly_count_lands_on_leap_days_only() {
        let rule = Frequency::from_rrule("FREQ=YEARLY;COUNT=3").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2024, 2, 29, 8),
                Duration::hours(1),
                rule,
                at(2024, 1, 1, 0),
                at(2033, 1, 1, 0)
            )),
            vec![d(2024, 2, 29), d(2028, 2, 29), d(2032, 2, 29)]
        );
    }

    #[test]
    fn daily_weekdays_skip_the_weekend() {
        let rule = Frequency::from_rrule("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR").unwrap();
        assert_eq!(
            occurrence_days(&occurrences_in(
                at(2026, 7, 8, 9),
                Duration::hours(1),
                rule,
                at(2026, 7, 8, 0),
                at(2026, 7, 14, 0)
            )),
            vec![d(2026, 7, 8), d(2026, 7, 9), d(2026, 7, 10), d(2026, 7, 13)]
        );
    }

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
        assert_eq!(Frequency::from_rrule("FREQ=WEEKLY;BYSETPOS=2"), None);
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
    fn a_custom_all_day_rule_uses_the_calendar_day_end_across_dst() {
        // COUNT keeps this on the custom path; an interval of 1 with nothing
        // else would collapse to a plain daily rule.
        let rule = Frequency::from_rrule("FREQ=DAILY;COUNT=40").unwrap();
        for (base, range_start, range_end, expected) in [
            (
                at(2026, 3, 1, 0),
                at(2026, 3, 9, 0),
                at(2026, 3, 10, 0),
                at(2026, 3, 9, 0),
            ),
            (
                at(2026, 10, 25, 0),
                at(2026, 11, 1, 23),
                at(2026, 11, 2, 0),
                at(2026, 11, 1, 0),
            ),
        ] {
            let occurrences = occurrences_with_end_in(
                base,
                Duration::days(1),
                rule,
                range_start,
                range_end,
                |start| crate::date_util::day_start_in(&NY, start.date_naive() + Duration::days(1)),
            );
            assert_eq!(occurrences, vec![expected]);
        }
    }

    #[test]
    fn all_day_occurrences_use_midnight_for_overlap_across_dst() {
        // An explicit timezone keeps this regression covered even on UTC CI.
        for (base, range_start, range_end, expected) in [
            (
                at(2026, 3, 1, 0),
                at(2026, 3, 9, 0),
                at(2026, 3, 10, 0),
                at(2026, 3, 9, 0),
            ),
            (
                at(2026, 10, 25, 0),
                at(2026, 11, 1, 23),
                at(2026, 11, 2, 0),
                at(2026, 11, 1, 0),
            ),
        ] {
            let occurrences = occurrences_with_end_in(
                base,
                Duration::days(1),
                Frequency::Daily,
                range_start,
                range_end,
                |start| crate::date_util::day_start_in(&NY, start.date_naive() + Duration::days(1)),
            );
            assert_eq!(occurrences, vec![expected]);
        }
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

    #[test]
    fn an_event_longer_than_its_interval_keeps_every_occurrence_still_running() {
        // A ten-day event that repeats daily has ten of them in flight on any
        // given day, and every one of them belongs in a range that day covers.
        let occ = occurrences_in(
            at(2026, 8, 1, 9),
            Duration::days(10),
            Frequency::Daily,
            at(2026, 8, 20, 0),
            at(2026, 8, 21, 0),
        );
        // The 10th is the earliest still running: it ends 9am on the 20th.
        assert_eq!(
            occurrence_days(&occ),
            (10..=20)
                .map(|d| NaiveDate::from_ymd_opt(2026, 8, d).unwrap())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_monthly_event_spanning_months_keeps_the_earlier_occurrence_still_running() {
        // Starts the 5th, runs 100 days: the June range is covered by the March,
        // April and May occurrences, which all overlap it.
        let occ = occurrences_in(
            at(2026, 1, 5, 9),
            Duration::days(100),
            Frequency::Monthly,
            at(2026, 6, 10, 0),
            at(2026, 6, 11, 0),
        );
        assert_eq!(
            occurrence_days(&occ),
            [(2026, 3, 5), (2026, 4, 5), (2026, 5, 5), (2026, 6, 5)]
                .map(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d).unwrap())
        );
    }

    #[test]
    fn a_multi_year_event_keeps_the_yearly_occurrence_still_running() {
        let occ = occurrences_in(
            at(2020, 6, 1, 9),
            Duration::days(800),
            Frequency::Yearly,
            at(2026, 1, 1, 0),
            at(2026, 1, 2, 0),
        );
        assert_eq!(
            occurrence_days(&occ),
            [(2024, 6, 1), (2025, 6, 1)].map(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d).unwrap())
        );
    }
}
