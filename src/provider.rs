//! The three sync providers, and how each is named to the user.
//!
//! Google, iCloud and CalDAV each had their own copy of the add/sync UI, which
//! meant three copies of every label, tooltip and error string — and they had
//! already drifted. Naming lives here instead, so the shared account plumbing
//! can talk about a provider without a `match` per sentence.
//!
//! Deliberately GTK-free: `key` is the value stored in the `accounts.provider`
//! column, so this is as much storage vocabulary as it is presentation.

/// A sync provider: its stored key and the words used for it in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Provider {
    /// The `accounts.provider` value in SQLite.
    pub key: &'static str,
    /// How the provider is named in buttons, toasts and the account list.
    pub label: &'static str,
    /// The noun counted in the post-sync summary toast. Google's is bare
    /// because its toast predates the others and reads as the default.
    pub calendar_noun: &'static str,
}

pub const GOOGLE: Provider = Provider {
    key: "google",
    label: "Google",
    calendar_noun: "calendar",
};

pub const ICLOUD: Provider = Provider {
    key: "icloud",
    label: "iCloud",
    calendar_noun: "iCloud calendar",
};

pub const CALDAV: Provider = Provider {
    key: "caldav",
    label: "CalDAV",
    calendar_noun: "CalDAV calendar",
};

/// Every provider, in the order their controls appear in the sidebar.
pub const ALL: [Provider; 3] = [GOOGLE, ICLOUD, CALDAV];

impl Provider {
    /// The provider for a stored `accounts.provider` value.
    pub fn from_key(key: &str) -> Option<Provider> {
        ALL.into_iter().find(|provider| provider.key == key)
    }

    /// How to name a stored provider value in the account list, falling back to
    /// the raw value so a row written by a newer version still reads sensibly.
    pub fn label_for_key(key: &str) -> &str {
        match Provider::from_key(key) {
            Some(provider) => provider.label,
            None => key,
        }
    }

    pub fn add_label(&self) -> String {
        format!("Add {}", self.label)
    }

    pub fn sync_label(&self) -> String {
        format!("Sync {}", self.label)
    }

    /// The sync button's tooltip. Before any account is connected the first
    /// sync is what discovers the calendars, so it's described differently.
    pub fn sync_tooltip(&self, has_accounts: bool) -> String {
        if has_accounts {
            format!(
                "Fetch the latest events from connected {} accounts",
                self.label
            )
        } else {
            format!("Fetch calendars from connected {} accounts", self.label)
        }
    }

    /// Shown when Sync is pressed with nothing connected.
    pub fn none_connected(&self) -> String {
        format!(
            "No {} accounts connected. Use {} first.",
            self.label,
            self.add_label()
        )
    }

    pub fn sync_failed(&self, error: &str) -> String {
        format!("{} sync failed: {}", self.label, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_provider_value_maps_to_its_provider() {
        assert_eq!(Provider::from_key("google"), Some(GOOGLE));
        assert_eq!(Provider::from_key("icloud"), Some(ICLOUD));
        assert_eq!(Provider::from_key("caldav"), Some(CALDAV));
        assert_eq!(Provider::from_key("fastmail"), None);
    }

    #[test]
    fn an_unknown_provider_value_is_shown_as_itself() {
        // A row from a newer version shouldn't render as blank in the list.
        assert_eq!(Provider::label_for_key("google"), "Google");
        assert_eq!(Provider::label_for_key("fastmail"), "fastmail");
    }

    #[test]
    fn the_button_labels_read_as_they_always_have() {
        assert_eq!(GOOGLE.add_label(), "Add Google");
        assert_eq!(GOOGLE.sync_label(), "Sync Google");
        assert_eq!(ICLOUD.add_label(), "Add iCloud");
        assert_eq!(ICLOUD.sync_label(), "Sync iCloud");
        assert_eq!(CALDAV.add_label(), "Add CalDAV");
        assert_eq!(CALDAV.sync_label(), "Sync CalDAV");
    }

    #[test]
    fn the_sync_tooltip_distinguishes_a_first_sync_from_a_refresh() {
        assert_eq!(
            GOOGLE.sync_tooltip(true),
            "Fetch the latest events from connected Google accounts"
        );
        assert_eq!(
            GOOGLE.sync_tooltip(false),
            "Fetch calendars from connected Google accounts"
        );
    }

    #[test]
    fn the_nothing_connected_message_points_at_the_add_button_by_name() {
        assert_eq!(
            ICLOUD.none_connected(),
            "No iCloud accounts connected. Use Add iCloud first."
        );
        assert_eq!(
            CALDAV.none_connected(),
            "No CalDAV accounts connected. Use Add CalDAV first."
        );
    }

    #[test]
    fn a_sync_failure_is_prefixed_with_the_provider() {
        assert_eq!(
            CALDAV.sync_failed("401 Unauthorized"),
            "CalDAV sync failed: 401 Unauthorized"
        );
    }
}
