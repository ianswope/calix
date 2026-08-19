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
    /// Accounts that failed as a whole — a missing credential, a rejected
    /// authorization, a discovery error — already rendered as
    /// `"<account>: <reason>"`. Separate from `failed` so the summary can say
    /// which accounts never ran at all, not just which calendars went stale.
    pub failed_accounts: Vec<String>,
}

impl SyncOutcome {
    pub fn record_success(&mut self) {
        self.synced += 1;
    }

    pub fn record_failure(&mut self, calendar: impl Into<String>) {
        self.failed.push(calendar.into());
    }

    pub fn record_account_failure(&mut self, account: impl Into<String>, error: impl Into<String>) {
        self.failed_accounts
            .push(format!("{}: {}", account.into(), error.into()));
    }

    /// Folds another account's outcome into this one, for the multi-account
    /// sync loops.
    pub fn merge(&mut self, other: SyncOutcome) {
        self.synced += other.synced;
        self.failed.extend(other.failed);
        self.failed_accounts.extend(other.failed_accounts);
    }

    /// Whether a finished sync is worth telling the user about: a manual one
    /// always is, a quiet one only when something went wrong.
    ///
    /// Gated on the same pair of collections [`Self::failure_note`] renders, so
    /// an account that never ran at all — a revoked credential, a secret missing
    /// from the keyring, a discovery error — can't leave every calendar it owns
    /// silently stale.
    pub fn needs_reporting(&self, quiet: bool) -> bool {
        !quiet || self.failure_note().is_some()
    }

    /// A trailing clause naming the calendars that failed, or `None` when
    /// everything synced. Callers append it to their success message so a
    /// partial failure never reads as a clean success.
    pub fn failure_note(&self) -> Option<String> {
        let mut notes = Vec::new();
        for account in &self.failed_accounts {
            notes.push(format!("couldn't sync {account}"));
        }
        if !self.failed.is_empty() {
            notes.push(format!(
                "couldn't sync {}: {}",
                self.failed.len(),
                self.failed.join(", ")
            ));
        }
        (!notes.is_empty()).then(|| notes.join(" — "))
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
            "Synced {} {noun}(s) from {} account(s)",
            self.synced,
            account_count.saturating_sub(self.failed_accounts.len())
        ))
    }
}

/// Toast text for an account that connected and was saved, but whose first
/// sync failed.
///
/// The account row and its secret are already written by this point, so
/// reporting a bare "connect failed" would hide a real account the user can
/// see in the sidebar and sync by hand.
pub fn added_but_not_synced(display_name: &str, error: &str) -> String {
    format!("Added {display_name}, but the first sync failed: {error}")
}

/// Syncs every account, isolating failures to the account that caused them.
///
/// A missing credential or an account-level network error must not abandon the
/// accounts after it: the account lists are ordered by display name, so an
/// early failure would otherwise starve the same accounts on every automatic
/// pass. `name` supplies the user-facing account name for the report — the
/// per-account errors themselves must not repeat it.
pub fn sync_accounts<A>(
    accounts: &[A],
    name: impl Fn(&A) -> String,
    sync_one: impl Fn(&A) -> Result<SyncOutcome, String>,
) -> SyncOutcome {
    let mut outcome = SyncOutcome::default();
    for account in accounts {
        let name = name(account);
        match sync_one(account) {
            Ok(account_outcome) => outcome.merge(account_outcome),
            Err(error) => {
                eprintln!("calix: failed to sync account {name}: {error}");
                outcome.record_account_failure(name, error);
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two accounts, the first of which fails outright.
    fn sync_two_accounts() -> SyncOutcome {
        sync_accounts(
            &["Broken", "Working"],
            |account| (*account).to_string(),
            |account| {
                if *account == "Broken" {
                    return Err("missing saved password".to_string());
                }
                let mut outcome = SyncOutcome::default();
                outcome.record_success();
                outcome.record_success();
                Ok(outcome)
            },
        )
    }

    #[test]
    fn a_failed_first_sync_still_reports_the_account_as_added() {
        assert_eq!(
            added_but_not_synced("Work", "the server took too long to respond"),
            "Added Work, but the first sync failed: the server took too long to respond"
        );
    }

    #[test]
    fn a_broken_account_does_not_stop_the_accounts_after_it() {
        let outcome = sync_two_accounts();
        assert_eq!(outcome.synced, 2);
    }

    #[test]
    fn a_broken_account_is_recorded_with_its_reason() {
        let outcome = sync_two_accounts();
        assert_eq!(
            outcome.failed_accounts,
            vec!["Broken: missing saved password".to_string()]
        );
    }

    #[test]
    fn a_failed_account_is_named_in_the_summary_and_left_out_of_the_count() {
        let outcome = sync_two_accounts();
        assert_eq!(
            outcome.synced_summary("calendar", 2),
            "Synced 2 calendar(s) from 1 account(s) \
             — couldn't sync Broken: missing saved password"
        );
    }

    #[test]
    fn account_and_calendar_failures_are_reported_side_by_side() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_failure("Birthdays");
        outcome.record_account_failure("Personal", "the server took too long to respond");
        assert_eq!(
            outcome.failure_note().as_deref(),
            Some(
                "couldn't sync Personal: the server took too long to respond \
                 — couldn't sync 1: Birthdays"
            )
        );
    }

    #[test]
    fn a_quiet_sync_reports_an_account_that_failed_as_a_whole() {
        let outcome = sync_two_accounts();
        assert!(outcome.failed.is_empty(), "no calendar failed on its own");
        assert!(outcome.needs_reporting(true));
    }

    #[test]
    fn a_quiet_sync_reports_a_calendar_that_went_stale() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        outcome.record_failure("Birthdays");
        assert!(outcome.needs_reporting(true));
    }

    #[test]
    fn a_quiet_sync_stays_quiet_when_everything_synced() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        assert!(!outcome.needs_reporting(true));
    }

    #[test]
    fn a_manual_sync_reports_even_a_clean_run() {
        let mut outcome = SyncOutcome::default();
        outcome.record_success();
        assert!(outcome.needs_reporting(false));
    }

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
