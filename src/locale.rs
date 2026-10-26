//! Locale-dependent date names, and the locale's first day of the week. GLib
//! and GTK have no API for the latter, so it reads glibc, as GtkCalendar does.

use crate::date_util;
use chrono::{Datelike, Days, NaiveDate, NaiveTime, Weekday};
use gtk::glib;

/// "October 2026". `%OB` is the standalone month name in languages that
/// inflect it (Polish "październik", not "października").
pub fn month_title(date: NaiveDate) -> String {
    format(date, "%OB %Y")
}

/// "October", standalone form.
pub fn month_name(date: NaiveDate) -> String {
    format(date, "%OB")
}

/// "Sunday, October 11, 2026". The day and month trade places when the
/// locale's date format writes the day first.
pub fn day_title(date: NaiveDate) -> String {
    date_util::long_date(&named_date(date), current_order())
}

/// "Oct 5 – 11, 2026", naming both months when the week crosses one.
pub fn week_title(start: NaiveDate, end: NaiveDate) -> String {
    date_util::week_range(&named_date(start), &named_date(end), current_order())
}

/// "Thursday, July 23". Notification text and the year-view tooltip.
pub fn notification_date(date: NaiveDate) -> String {
    date_util::weekday_and_date(&named_date(date), current_order())
}

/// "Mon, Aug 10, 2026", or without the year when a range prints it once.
pub fn short_date(date: NaiveDate, include_year: bool) -> String {
    date_util::short_date(&named_date(date), current_order(), include_year)
}

/// "Oct 26, 4:07 PM", or "Oct 26, 16:07" on a 24-hour clock. The clock text
/// is already formatted. The month name comes from the locale, and the day
/// and month trade places with it.
pub fn updated_stamp(date: NaiveDate, time: &str) -> String {
    let named = named_date(date);
    date_util::updated_stamp(&named.month_abbrev, named.day, current_order(), time)
}

/// The locale's clock, from its POSIX `t_fmt`.
pub fn clock() -> date_util::Clock {
    date_util::clock_from_format(&system_time_format())
}

/// "4:07 PM" or "16:07", with no leading zero and no seconds.
pub fn format_time(time: NaiveTime) -> String {
    date_util::format_time(time, clock())
}

fn named_date(date: NaiveDate) -> date_util::NamedDate {
    date_util::NamedDate {
        weekday: format(date, "%A"),
        weekday_abbrev: format(date, "%a"),
        month: format(date, "%B"),
        month_abbrev: format(date, "%b"),
        month_number: date.month(),
        day: date.day(),
        year: date.year(),
    }
}

fn current_order() -> date_util::DateOrder {
    date_util::date_order(&system_date_format())
}

/// "Mon".
pub fn weekday_abbreviation(day: Weekday) -> String {
    // 2023-01-01 was a Sunday.
    let sunday = NaiveDate::from_ymd_opt(2023, 1, 1).expect("valid date");
    format(sunday + Days::new(day.num_days_from_sunday().into()), "%a")
}

/// "M".
pub fn weekday_initial(day: Weekday) -> String {
    let abbreviation = weekday_abbreviation(day);
    abbreviation
        .chars()
        .take(1)
        .flat_map(char::to_uppercase)
        .collect()
}

fn format(date: NaiveDate, format: &str) -> String {
    glib::DateTime::from_local(
        date.year(),
        date.month() as i32,
        date.day() as i32,
        12,
        0,
        0.0,
    )
    .and_then(|date| date.format(format))
    .map(String::from)
    .unwrap_or_default()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn first_weekday() -> Option<Weekday> {
    use std::ffi::{c_char, c_int};

    // _NL_ITEM(LC_TIME, 0x66) and _NL_ITEM(LC_TIME, 0x68) from <langinfo.h>.
    const NL_TIME_WEEK_1STDAY: c_int = 0x20066;
    const NL_TIME_FIRST_WEEKDAY: c_int = 0x20068;

    unsafe extern "C" {
        fn nl_langinfo(item: c_int) -> *const c_char;
    }

    // WEEK_1STDAY is a word glibc returns in place of the pointer, through a
    // union. Reading it back through one, as GtkCalendar does, takes the right
    // half of the pointer on either byte order.
    #[repr(C)]
    union Word {
        string: *const c_char,
        word: u32,
    }

    // SAFETY: both items exist in every glibc locale, and FIRST_WEEKDAY points
    // at a byte.
    let (week_1stday, first_weekday) = unsafe {
        (
            Word {
                string: nl_langinfo(NL_TIME_WEEK_1STDAY),
            }
            .word,
            *nl_langinfo(NL_TIME_FIRST_WEEKDAY) as u8,
        )
    };
    crate::date_util::first_weekday_from_langinfo(week_1stday, first_weekday)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn first_weekday() -> Option<Weekday> {
    None
}

/// The locale's `d_fmt`, such as `%m/%d/%Y` or `%d.%m.%Y`. Empty where the
/// platform has no langinfo, which keeps month-first order.
fn system_date_format() -> String {
    // `D_FMT` is `_NL_ITEM(LC_TIME, 41)`, and `LC_TIME` is 2.
    system_langinfo((2 << 16) | 41)
}

/// The locale's `t_fmt`, such as `%H:%M:%S` or `%r`. Empty where the platform
/// has no langinfo, which keeps the 12-hour clock.
fn system_time_format() -> String {
    // `T_FMT` is the item after `D_FMT`.
    system_langinfo((2 << 16) | 42)
}

fn system_langinfo(item: i32) -> String {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::ffi::{CStr, c_char, c_int};

        unsafe extern "C" {
            fn nl_langinfo(item: c_int) -> *const c_char;
        }

        let pointer = unsafe { nl_langinfo(item) };
        if pointer.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        let _ = item;
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nothing in the test binary calls setlocale outside the ignored GTK
    // tests, so these see the C locale.
    #[test]
    fn month_names_and_titles_come_from_the_locale() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(month_title(date), "October 2026");
        assert_eq!(month_name(date), "October");
    }

    #[test]
    fn weekday_names_come_from_the_locale() {
        assert_eq!(weekday_abbreviation(Weekday::Sun), "Sun");
        assert_eq!(weekday_abbreviation(Weekday::Thu), "Thu");
        assert_eq!(weekday_initial(Weekday::Thu), "T");
    }

    #[test]
    fn a_day_title_uses_the_locale_names() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(day_title(date), "Sunday, October 11, 2026");
    }

    #[test]
    fn a_week_title_uses_the_locale_month_names() {
        let start = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(week_title(start, end), "Oct 5 – 11, 2026");
    }

    #[test]
    fn a_notification_date_uses_the_locale_weekday_and_month() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        assert_eq!(notification_date(date), "Thursday, July 23");
    }

    #[test]
    fn an_account_update_uses_the_locale_month_abbreviation() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert_eq!(updated_stamp(date, "4:07 PM"), "Oct 26, 4:07 PM");
    }

    #[test]
    fn the_c_locale_uses_a_24_hour_clock() {
        // Nothing calls setlocale in this binary, so this is the C locale.
        // Its `t_fmt` is `%H:%M:%S`.
        assert_eq!(clock(), date_util::Clock::TwentyFourHour);
        assert_eq!(
            format_time(NaiveTime::from_hms_opt(16, 7, 0).unwrap()),
            "16:07"
        );
    }
}
