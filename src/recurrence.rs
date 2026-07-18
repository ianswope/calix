//! Event recurrence rules.
//!
//! Calix models a small set of simple repeat frequencies that round-trip to an
//! iCalendar `RRULE` value — the representation both Google Calendar (a
//! `recurrence` array of `"RRULE:…"` strings) and CalDAV (an `RRULE:` line in
//! the `VEVENT`) speak natively. An event's recurrence is an
//! [`Option<Frequency>`]; `None` is a one-off event. Richer rules (intervals,
//! `BYDAY`, `UNTIL`/`COUNT`) can grow from here.

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
}
