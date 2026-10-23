//! Locale-dependent date names, and the locale's first day of the week. GLib
//! and GTK have no API for the latter, so it reads glibc, as GtkCalendar does.

use chrono::{Datelike, Days, NaiveDate, Weekday};
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
}
