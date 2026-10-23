//! The locale's first day of the week. GLib and GTK have no API for it, so this
//! reads glibc directly, as GtkCalendar does.

use chrono::Weekday;

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
