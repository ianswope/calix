//! The custom repeat controls. Validation, the subtitle, and expansion live in
//! [`crate::recurrence`].

use crate::date_util::{first_weekday, weekdays_from};
use crate::recurrence::{CustomFreq, CustomRepeatSpec, CustomRule, Frequency};
use adw::prelude::*;
use chrono::{NaiveDate, Weekday};

const DATE_FORMAT: &str = "%Y-%m-%d";
const DATE_ERROR: &str = "Enter the stop date as YYYY-MM-DD";

pub(crate) struct CustomRepeat {
    pub row: adw::ExpanderRow,
    frequency: adw::ComboRow,
    interval: adw::SpinRow,
    days: adw::ActionRow,
    day_choices: adw::PreferencesRow,
    weekdays: Vec<(Weekday, gtk::CheckButton)>,
    ordinal: adw::ComboRow,
    ends: adw::ComboRow,
    count: adw::SpinRow,
    until: adw::EntryRow,
}

impl CustomRepeat {
    pub fn new() -> Self {
        let row = adw::ExpanderRow::builder()
            .title("Custom repeat")
            .expanded(true)
            .build();
        row.set_subtitle_lines(2);
        let frequency = adw::ComboRow::builder()
            .title("Frequency")
            .model(&gtk::StringList::new(&[
                "Daily", "Weekly", "Monthly", "Yearly",
            ]))
            .selected(CustomFreq::Weekly.index())
            .build();
        let interval = adw::SpinRow::with_range(1.0, 999.0, 1.0);
        interval.set_title(interval_title(CustomFreq::Weekly));
        interval.set_value(2.0);
        let days = adw::ActionRow::builder().title("On these days").build();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        buttons.set_margin_top(6);
        buttons.set_margin_bottom(6);
        buttons.set_margin_start(12);
        buttons.set_margin_end(12);
        // Same order as the month and week headings, so Sunday-first locales
        // don't meet a Monday-first row here. Bits stay Monday-based.
        let weekdays = weekdays_from(first_weekday())
            .into_iter()
            .map(|day| {
                let button =
                    gtk::CheckButton::with_label(&crate::locale::weekday_abbreviation(day));
                buttons.append(&button);
                (day, button)
            })
            .collect();
        // A separate row keeps all seven choices reachable in a narrow dialog.
        let day_choices = adw::PreferencesRow::new();
        day_choices.set_child(Some(&buttons));
        let ordinal = adw::ComboRow::builder()
            .title("Week of the month")
            .model(&gtk::StringList::new(&[
                "Same date",
                "Each selected weekday",
                "First",
                "Second",
                "Third",
                "Fourth",
                "Last",
            ]))
            .build();
        let ends = adw::ComboRow::builder()
            .title("Ends")
            .model(&gtk::StringList::new(&[
                "Never",
                "After a number of events",
                "On a date",
            ]))
            .build();
        let count = adw::SpinRow::with_range(1.0, 999.0, 1.0);
        count.set_title("Number of events");
        count.set_value(10.0);
        let until = adw::EntryRow::builder()
            .title("Stop on (YYYY-MM-DD)")
            .build();
        for child in [
            frequency.upcast_ref::<gtk::Widget>(),
            interval.upcast_ref(),
            ordinal.upcast_ref(),
            days.upcast_ref(),
            day_choices.upcast_ref(),
            ends.upcast_ref(),
            count.upcast_ref(),
            until.upcast_ref(),
        ] {
            row.add_row(child);
        }

        let repeat = Self {
            row,
            frequency,
            interval,
            days,
            day_choices,
            weekdays,
            ordinal,
            ends,
            count,
            until,
        };
        repeat.watch();
        repeat.sync();
        repeat
    }

    pub fn set_rule(&self, rule: CustomRule) {
        self.frequency.set_selected(rule.frequency.index());
        // A rule written outside the spin's usual range still has to come
        // back unchanged. The spin would otherwise clamp it.
        self.interval
            .set_range(1.0, f64::from(rule.interval).max(999.0));
        self.interval.set_value(f64::from(rule.interval));
        for (day, button) in &self.weekdays {
            button.set_active(rule.weekdays & weekday_bit(*day) != 0);
        }
        self.ordinal.set_selected(ordinal_index(rule));
        match (rule.count, rule.until) {
            (Some(count), None) => {
                self.ends.set_selected(1);
                self.count.set_range(1.0, f64::from(count).max(999.0));
                self.count.set_value(f64::from(count));
                self.until.set_text("");
            }
            (None, Some(until)) => {
                self.ends.set_selected(2);
                self.until.set_text(&until.format(DATE_FORMAT).to_string());
            }
            _ => {
                self.ends.set_selected(0);
                self.until.set_text("");
            }
        }
        self.sync();
    }

    pub fn value(&self) -> Result<Frequency, &'static str> {
        let until = if self.ends.selected() == 2 {
            let text = self.until.text();
            let text = text.trim();
            if text.is_empty() {
                return Err(DATE_ERROR);
            }
            Some(NaiveDate::parse_from_str(text, DATE_FORMAT).map_err(|_| DATE_ERROR)?)
        } else {
            None
        };
        self.spec(until).build()
    }

    fn spec(&self, until: Option<NaiveDate>) -> CustomRepeatSpec {
        let frequency =
            CustomFreq::from_index(self.frequency.selected()).unwrap_or(CustomFreq::Weekly);
        let monthly_days = frequency == CustomFreq::Monthly && self.ordinal.selected() != 0;
        let weekdays = if frequency == CustomFreq::Weekly || monthly_days {
            self.weekdays
                .iter()
                .filter(|(_, button)| button.is_active())
                .fold(0u8, |bits, (day, _)| bits | weekday_bit(*day))
        } else {
            0
        };
        CustomRepeatSpec {
            frequency,
            interval: self.interval.value() as u16,
            weekdays,
            // "Each selected weekday" is ordinal 0, and so is "no ordinal".
            // With no day chosen that would be saved as "same date", so treat
            // it as a numbered week that's missing its days.
            ordinal: if monthly_days && self.ordinal.selected() == 1 && weekdays == 0 {
                1
            } else if monthly_days {
                ordinal_value(self.ordinal.selected())
            } else {
                0
            },
            count: (self.ends.selected() == 1).then_some(self.count.value() as u32),
            until,
        }
    }

    fn sync(&self) {
        let frequency =
            CustomFreq::from_index(self.frequency.selected()).unwrap_or(CustomFreq::Weekly);
        let monthly = frequency == CustomFreq::Monthly;
        let by_weekday = monthly && self.ordinal.selected() != 0;
        self.interval.set_title(interval_title(frequency));
        self.ordinal.set_visible(monthly);
        self.days
            .set_visible(frequency == CustomFreq::Weekly || by_weekday);
        self.day_choices
            .set_visible(frequency == CustomFreq::Weekly || by_weekday);
        self.days.set_subtitle(if by_weekday {
            "Choose one or more"
        } else {
            "Leave empty to use the event's weekday"
        });
        self.count.set_visible(self.ends.selected() == 1);
        self.until.set_visible(self.ends.selected() == 2);
        let until = parsed_until(self.ends.selected(), &self.until.text());
        let summary = match until {
            Err(message) => message.to_string(),
            Ok(day) => self.spec(day).summary(),
        };
        self.row.set_subtitle(&summary);
    }

    fn watch(&self) {
        let refresh = std::rc::Rc::new({
            let row = self.row.downgrade();
            let frequency = self.frequency.downgrade();
            let interval = self.interval.downgrade();
            let days = self.days.downgrade();
            let day_choices = self.day_choices.downgrade();
            let ordinal = self.ordinal.downgrade();
            let ends = self.ends.downgrade();
            let count = self.count.downgrade();
            let until = self.until.downgrade();
            let weekday_ids: Vec<Weekday> = self.weekdays.iter().map(|(day, _)| *day).collect();
            let weekday_weak: Vec<_> = self
                .weekdays
                .iter()
                .map(|(_, button)| button.downgrade())
                .collect();
            move || {
                let Some(row) = row.upgrade() else { return };
                let Some(frequency) = frequency.upgrade() else {
                    return;
                };
                let Some(interval) = interval.upgrade() else {
                    return;
                };
                let Some(days) = days.upgrade() else { return };
                let Some(day_choices) = day_choices.upgrade() else {
                    return;
                };
                let Some(ordinal) = ordinal.upgrade() else {
                    return;
                };
                let Some(ends) = ends.upgrade() else { return };
                let Some(count) = count.upgrade() else { return };
                let Some(until) = until.upgrade() else { return };
                let weekdays: Vec<_> = weekday_ids
                    .iter()
                    .zip(&weekday_weak)
                    .filter_map(|(day, button)| button.upgrade().map(|button| (*day, button)))
                    .collect();
                if weekdays.len() != 7 {
                    return;
                }
                // Rebuild a view of the controls and reuse the same sync the
                // constructor uses. The widgets are the ones already on screen.
                let repeat = CustomRepeat {
                    row,
                    frequency,
                    interval,
                    days,
                    day_choices,
                    weekdays,
                    ordinal,
                    ends,
                    count,
                    until,
                };
                repeat.sync();
            }
        });
        for widget in [
            self.frequency.upcast_ref::<gtk::Widget>(),
            self.ordinal.upcast_ref(),
            self.ends.upcast_ref(),
        ] {
            let refresh = refresh.clone();
            widget.connect_notify_local(Some("selected"), move |_, _| refresh());
        }
        let refresh_interval = refresh.clone();
        self.interval
            .connect_value_notify(move |_| refresh_interval());
        let refresh_count = refresh.clone();
        self.count.connect_value_notify(move |_| refresh_count());
        let refresh_until = refresh.clone();
        self.until.connect_changed(move |_| refresh_until());
        for (_, button) in &self.weekdays {
            let refresh = refresh.clone();
            button.connect_toggled(move |_| refresh());
        }
    }
}

fn weekday_bit(day: Weekday) -> u8 {
    1u8 << day.num_days_from_monday()
}

fn interval_title(frequency: CustomFreq) -> &'static str {
    match frequency {
        CustomFreq::Daily => "Repeat every (days)",
        CustomFreq::Weekly => "Repeat every (weeks)",
        CustomFreq::Monthly => "Repeat every (months)",
        CustomFreq::Yearly => "Repeat every (years)",
    }
}

/// Combo index for a stored rule. "Same date" is 0; "each selected weekday"
/// is 1; the numbered weeks follow, with last at the end.
fn ordinal_index(rule: CustomRule) -> u32 {
    if rule.frequency != CustomFreq::Monthly || rule.weekdays == 0 {
        return 0;
    }
    match rule.ordinal {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 5,
        -1 => 6,
        _ => 1,
    }
}

fn ordinal_value(index: u32) -> i8 {
    match index {
        2 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        6 => -1,
        _ => 0,
    }
}

fn parsed_until(ends: u32, text: &str) -> Result<Option<NaiveDate>, &'static str> {
    if ends != 2 {
        return Ok(None);
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(DATE_ERROR);
    }
    NaiveDate::parse_from_str(text, DATE_FORMAT)
        .map(Some)
        .map_err(|_| DATE_ERROR)
}
