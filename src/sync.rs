//! Shared reporting type for the Google and CalDAV sync loops.

/// The result of syncing one or more accounts' calendars: how many calendars
/// were fetched and stored successfully, and the names of any that failed.
///
/// A per-calendar fetch error is not a reason to abort the whole sync, but it
/// also isn't a success — carrying the failures separately lets the UI report
/// "X of Y" and name what went stale instead of an unqualified "Synced Y".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub synced: usize,
    pub failed: Vec<String>,
}

impl SyncOutcome {
    pub fn record_success(&mut self) {
        self.synced += 1;
    }

    pub fn record_failure(&mut self, calendar: impl Into<String>) {
        self.failed.push(calendar.into());
    }

    /// Folds another account's outcome into this one, for the multi-account
    /// sync loops.
    pub fn merge(&mut self, other: SyncOutcome) {
        self.synced += other.synced;
        self.failed.extend(other.failed);
    }

    /// A trailing clause naming the calendars that failed, or `None` when
    /// everything synced. Callers append it to their success message so a
    /// partial failure never reads as a clean success.
    pub fn failure_note(&self) -> Option<String> {
        (!self.failed.is_empty()).then(|| {
            format!(
                "couldn't sync {}: {}",
                self.failed.len(),
                self.failed.join(", ")
            )
        })
    }

    fn with_failure_note(&self, base: String) -> String {
        match self.failure_note() {
            Some(note) => format!("{base} — {note}"),
            None => base,
        }
    }

    /// Toast text for adding an account, e.g.
    /// `Added Work and synced 3 calendar(s)`.
    pub fn added_summary(&self, display_name: &str, noun: &str) -> String {
        self.with_failure_note(format!(
            "Added {display_name} and synced {} {noun}(s)",
            self.synced
        ))
    }

    /// Toast text for a manual/automatic sync across `account_count` accounts,
    /// e.g. `Synced 3 calendar(s) from 1 account(s)`.
    pub fn synced_summary(&self, noun: &str, account_count: usize) -> String {
        self.with_failure_note(format!(
            "Synced {} {noun}(s) from {account_count} account(s)",
            self.synced
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_sync_has_no_failure_note() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_success();
        assert_eq!(outcome.synced, 2);
        assert_eq!(outcome.failure_note(), None);
    }

    #[test]
    fn a_partial_failure_names_the_stale_calendars() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_failure("Work");
        outcome.record_failure("Birthdays");
        assert_eq!(outcome.synced, 1);
        assert_eq!(
            outcome.failure_note().as_deref(),
            Some("couldn't sync 2: Work, Birthdays")
        );
    }

    #[test]
    fn merge_combines_counts_and_failures_across_accounts() {
        let mut first = SyncOutcome::default();
        first.record_success();
        first.record_failure("Shared");
        let mut second = SyncOutcome::default();
        second.record_success();
        second.record_success();
        first.merge(second);
        assert_eq!(first.synced, 3);
        assert_eq!(first.failed, vec!["Shared".to_string()]);
    }

    #[test]
    fn summaries_stay_clean_when_nothing_failed() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_success();
        outcome.record_success();
        assert_eq!(
            outcome.added_summary("Work", "calendar"),
            "Added Work and synced 3 calendar(s)"
        );
        assert_eq!(
            outcome.synced_summary("iCloud calendar", 1),
            "Synced 3 iCloud calendar(s) from 1 account(s)"
        );
    }

    #[test]
    fn summaries_call_out_partial_failures() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_failure("Work");
        assert_eq!(
            outcome.added_summary("Home", "calendar"),
            "Added Home and synced 1 calendar(s) — couldn't sync 1: Work"
        );
        assert_eq!(
            outcome.synced_summary("calendar", 2),
            "Synced 1 calendar(s) from 2 account(s) — couldn't sync 1: Work"
        );
    }
}
